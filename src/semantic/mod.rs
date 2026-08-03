//! Semantic (embedding) search over schema records.
//!
//! Embeds `path + description + type` per record and the query with a local
//! `all-MiniLM-L6-v2` model (pipeline borrowed from ae), truncates to 64-d
//! vectors, and ranks by cosine. Falls back to a deterministic hash
//! embedder when the model can't be fetched, so it always runs.
//!
//! v0 embeds every record per invocation (fine at schema scale). A persistent
//! embedding cache keyed by schema hash — like ae's SQLite store — is the
//! follow-up for very large / federated schemas; the embed and rank steps are
//! kept separable here so that cache drops in cleanly.

mod cache;
mod embed;
mod mrl;

use crate::model::SchemaRecord;
use embed::{default_embedder, Embedder};
use mrl::{compress_matryoshka_vector, cosine_similarity};

/// Drop hits below this fraction of the top cosine. Deliberately mild:
/// measured cosine curves decay smoothly (no tiers, no cliff), and near the
/// noise floor relevant and irrelevant records interleave — so this bounds
/// the deep tail (`-l 500` returning 500 rows of monotonic noise), it does
/// not judge relevance.
const TAIL_CUTOFF: f64 = 0.7;

/// Apply [`TAIL_CUTOFF`] relative to the best hit. Skipped when the top score
/// isn't positive — a floor computed from a negative top would drop even the
/// top itself.
fn bound_tail<T>(hits: &mut Vec<(f64, T)>) {
    let Some(top) = hits.first().map(|h| h.0).filter(|t| *t > 0.0) else {
        return;
    };
    let floor = top * TAIL_CUTOFF;
    hits.retain(|(s, _)| *s >= floor);
}

/// Embed the query and each record, rank by cosine. Returns `(score, record)`
/// pairs, best first — the caller formats them exactly like fuzzy hits, so
/// `--json` / `--ndjson` work identically in both modes.
///
/// The model load is deliberately synchronous, not overlapped on a thread:
/// ONNX Runtime's C++ one-time init segfaults if the process exits mid-load
/// (a thread dropped on a fuzzy-only exit), and joining always would erase
/// the overlap. The exact-match skip in the CLI avoids the cost where it
/// matters instead.
pub fn search<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    filters: crate::search::Filters<'_>,
    limit: usize,
    model: Option<&str>,
    refresh: bool,
) -> Vec<(f64, &'a SchemaRecord)> {
    let mut model_span = crate::profile::span("semantic: model load");
    let embedder = default_embedder(model);
    model_span.note(|| embedder.kind().to_string());
    drop(model_span);
    // On the good path (onnx) this is verbose-only noise; a fall back to the
    // hash embedder means weaker results, so surface that unconditionally.
    if embedder.kind() == "onnx" {
        crate::detail!("semantic search via onnx embeddings");
    } else {
        crate::status!(
            "embedding model unavailable — semantic results will be weaker (-v for why)"
        );
    }

    // Per-record vectors are the expensive part; cache them keyed by schema
    // content + embedder + model, so editing the schema (or switching model)
    // changes the key and re-embeds automatically. `--refresh` forces a miss.
    let mut vec_span = crate::profile::span("semantic: vectors");
    let cache_path = cache::path(records, embedder.kind(), model);
    let cached = if refresh {
        None
    } else {
        cache_path
            .as_deref()
            .and_then(|p| cache::load(p, records.len()))
    };
    let vectors = match cached {
        Some(v) => {
            if let Some(p) = cache_path.as_deref() {
                crate::detail!("vector cache hit: {}", crate::paths::display(p));
                cache::touch(p); // LRU: mark this schema as recently used
            }
            v
        }
        None => {
            if let Some(p) = cache_path.as_deref() {
                crate::detail!("vector cache miss: {}", crate::paths::display(p));
            }
            use rayon::prelude::*;
            use std::io::IsTerminal;
            use std::sync::atomic::{AtomicUsize, Ordering};

            // A whole-schema miss doesn't mean the vectors are worthless: adding
            // a field invalidates the file's name, not the other 49,000 records'
            // embeddings. Reload recent files for this embedder and reuse every
            // vector whose text is unchanged, so routine drift costs inference
            // on the delta instead of the schema.
            let keys: Vec<cache::Key> = records
                .iter()
                .map(|r| cache::key(&record_text(r)))
                .collect();
            let reusable = cache::reusable(embedder.kind(), model);
            let pending: Vec<usize> = (0..records.len())
                .filter(|i| !reusable.contains_key(&keys[*i]))
                .collect();

            let total = pending.len();
            let reused = records.len() - total;
            crate::detail!(
                "vector cache: {reused} of {} vectors reused, {total} to embed",
                records.len()
            );
            if total > 0 {
                if reused > 0 {
                    crate::status!("embedding {total} new/changed records ({reused} reused)…");
                } else {
                    crate::status!("embedding {total} records (one-time; may take a minute)…");
                }
            }
            let done = AtomicUsize::new(0);

            // One embedder per worker thread, built on first use and reused for
            // every record that thread handles. rayon's `map_init` rebuilt it per
            // job-split (≈ once per record), reloading the ONNX session tens of
            // thousands of times on a large schema; a `thread_local` caps it at
            // one per worker. `model` is constant for the process, so the workers
            // all resolve the same embedder kind (no mixed onnx/hash vectors), and
            // caching on "already built?" alone is safe. Order-preserving
            // `collect` keeps vectors index-aligned with `records`.
            use std::cell::RefCell;
            thread_local! {
                static EMBEDDER: RefCell<Option<Box<dyn Embedder>>> = const { RefCell::new(None) };
            }
            let fresh: Vec<Vec<f32>> = std::thread::scope(|scope| {
                let show_progress =
                    std::io::stderr().is_terminal() && total > 500 && !crate::logging::is_quiet();
                if show_progress {
                    scope.spawn(|| loop {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        let d = done.load(Ordering::Relaxed);
                        eprint!("\rgqls: embedded {d}/{total}…    ");
                        if d >= total {
                            eprintln!();
                            break;
                        }
                    });
                }
                pending
                    .par_iter()
                    .map(|&i| {
                        let out = EMBEDDER.with(|cell| {
                            let mut slot = cell.borrow_mut();
                            let emb = slot.get_or_insert_with(|| default_embedder(model));
                            compress_matryoshka_vector(&emb.embed(&record_text(&records[i])))
                        });
                        done.fetch_add(1, Ordering::Relaxed);
                        out
                    })
                    .collect()
            });

            // Reassemble in record order from the two sources: freshly embedded
            // for the delta, reused for the rest. Two records with identical
            // text share a key and simply share the vector — reuse is a lookup,
            // not a removal, so the duplicate resolves the same way. Neither
            // branch can miss by construction; embed on the spot if one somehow
            // does, so a bug degrades to slow rather than to a zero vector that
            // would silently score as unrelated to everything.
            let mut minted: std::collections::HashMap<usize, Vec<f32>> =
                pending.into_iter().zip(fresh).collect();
            let v: Vec<Vec<f32>> = (0..records.len())
                .map(|i| {
                    minted
                        .remove(&i)
                        .or_else(|| reusable.get(&keys[i]).cloned())
                        .unwrap_or_else(|| {
                            compress_matryoshka_vector(&embedder.embed(&record_text(&records[i])))
                        })
                })
                .collect();
            if let Some(p) = cache_path.as_deref() {
                cache::store(p, &keys, &v);
                cache::prune(cache::max_files()); // evict least-recently-used
            }
            v
        }
    };

    vec_span.note(|| format!("{} vectors", vectors.len()));
    drop(vec_span);

    // warm() calls with an empty query and limit 0 purely to populate the cache
    // above — there's nothing to rank, so skip the query embed + cosine pass.
    if query.is_empty() && limit == 0 {
        return Vec::new();
    }

    let embed_span = crate::profile::span("semantic: query embed");
    let query_vec = compress_matryoshka_vector(&embedder.embed(query));
    drop(embed_span);

    let mut cosine_span = crate::profile::span("semantic: cosine");
    let predicate = filters.compile();
    let mut hits: Vec<(f64, &SchemaRecord)> = records
        .iter()
        .zip(&vectors)
        .filter(|(r, _)| predicate.accepts(r))
        .map(|(r, v)| (cosine_similarity(&query_vec, v) as f64, r))
        .collect();

    cosine_span.note(|| format!("{} scored", hits.len()));
    drop(cosine_span);

    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    bound_tail(&mut hits);
    hits.truncate(limit);
    hits
}

/// Split an identifier into the words it's built from: `cancelSubscription` →
/// `cancel Subscription`, `first_name` → `first name`, `Mutation.cancelUser` →
/// `Mutation cancel User`. The embedding model's tokenizer shreds camelCase into
/// meaningless word pieces (`cancelSubscription` is not a word it has ever seen),
/// so handing it real words is the difference between embedding the concept and
/// embedding spelling. Word boundaries come from the fuzzy scorer, so the two
/// halves of gqls agree on where a name's words start.
fn humanize(ident: &str) -> String {
    let chars: Vec<char> = ident.chars().collect();
    let boundary = crate::search::score::boundaries(&chars);
    let mut out = String::with_capacity(ident.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            // `.` / `_` / stray punctuation are separators, not signal
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if boundary[i] && !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push(c);
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The natural-language signal a semantic query matches against: the path plus
/// the human description and the type. Identifiers are split into words and the
/// type's `[]`/`!` wrappers dropped — punctuation and subword fragments are
/// tokens the model has to spend attention on for no meaning.
fn record_text(r: &SchemaRecord) -> String {
    let mut s = humanize(&r.path);
    if let Some(d) = &r.description {
        s.push_str(" — ");
        s.push_str(d);
    }
    if let Some(t) = r.base_type() {
        s.push_str(" : ");
        s.push_str(&humanize(t));
    }
    s
}

/// Delete all cached embedding vector files; returns how many were removed.
pub fn clear_cache() -> usize {
    cache::clear()
}

/// Whether the schema's real (ONNX) vectors are cached, so the default combine
/// path can run without a foreground embed. Deliberately ignores hash-fallback
/// vectors: a run re-selects the embedder and keys the cache on *its* kind, so
/// treating a stale `hash` cache as "warm" while the run picks `onnx` would
/// trigger exactly the surprise foreground embed this gate exists to avoid.
/// Checked without loading a model.
pub fn is_cached(records: &[SchemaRecord], model: Option<&str>) -> bool {
    cache::exists(records, "onnx", model)
}

/// Embed + cache every record's vector without running a real query — pre-warms
/// the cache so the first search is instant. Returns the record count.
pub fn warm(records: &[SchemaRecord], model: Option<&str>, refresh: bool) -> usize {
    let _ = search("", records, Default::default(), 0, model, refresh);
    records.len()
}

#[cfg(test)]
mod tests {
    use super::{bound_tail, humanize, record_text};
    use crate::model::{Kind, SchemaRecord};

    #[test]
    fn humanize_splits_identifiers_into_words() {
        assert_eq!(humanize("cancelSubscription"), "cancel Subscription");
        assert_eq!(humanize("Mutation.cancelUser"), "Mutation cancel User");
        assert_eq!(humanize("first_name"), "first name");
        // acronym runs break where the word does, not on every capital
        assert_eq!(humanize("URLSlug"), "URL Slug");
        assert_eq!(humanize("id"), "id");
    }

    #[test]
    fn record_text_drops_type_wrappers_and_splits_names() {
        let rec = SchemaRecord {
            path: "Query.myEmployer".into(),
            name: "myEmployer".into(),
            kind: Kind::Query,
            parent: Some("Query".into()),
            type_ref: Some("[CompanyProfile!]!".into()),
            description: Some("The employer".into()),
            deprecated: None,
            directives: vec![],
            possible_types: vec![],
            args: vec![],
        };
        assert_eq!(
            record_text(&rec),
            "Query my Employer — The employer : Company Profile"
        );
    }

    #[test]
    fn bounds_the_weak_tail_relative_to_the_top() {
        let mut hits = vec![(1.0, "a"), (0.8, "b"), (0.6, "c")];
        bound_tail(&mut hits);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn keeps_everything_when_the_top_is_not_positive() {
        let mut hits = vec![(-0.1, "a"), (-0.5, "b")];
        bound_tail(&mut hits);
        assert_eq!(hits.len(), 2);
    }
}
