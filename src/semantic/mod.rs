//! Semantic (embedding) search over schema records.
//!
//! Embeds `path + description + type` per record and the query with a local
//! `all-MiniLM-L6-v2` model (pipeline borrowed from ae), compresses to 64-d
//! Matryoshka vectors, and ranks by cosine. Falls back to a deterministic hash
//! embedder when the model can't be fetched, so it always runs.
//!
//! v0 embeds every record per invocation (fine at schema scale). A persistent
//! embedding cache keyed by schema hash — like ae's SQLite store — is the
//! follow-up for very large / federated schemas; the embed and rank steps are
//! kept separable here so that cache drops in cleanly.

mod cache;
mod embed;
mod mrl;

use crate::model::{Kind, SchemaRecord};
use embed::{default_embedder, Embedder};
use mrl::{compress_matryoshka_vector, cosine_similarity};

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
    kind: Option<Kind>,
    parent: Option<&str>,
    limit: usize,
    model: Option<&str>,
    refresh: bool,
) -> Vec<(f64, &'a SchemaRecord)> {
    let embedder = default_embedder(model);
    // On the good path (onnx) this is verbose-only noise; a fall back to the
    // hash embedder means weaker results, so surface that unconditionally.
    if embedder.kind() == "onnx" {
        crate::detail!("semantic search via onnx embeddings");
    } else {
        crate::status!(
            "semantic search via {} embeddings — model unavailable, results are weaker (-v for why)",
            embedder.kind()
        );
    }

    // Per-record vectors are the expensive part; cache them keyed by schema
    // content + embedder + model, so editing the schema (or switching model)
    // changes the key and re-embeds automatically. `--refresh` forces a miss.
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
                crate::detail!("vector cache hit: {}", p.display());
                cache::touch(p); // LRU: mark this schema as recently used
            }
            v
        }
        None => {
            if let Some(p) = cache_path.as_deref() {
                crate::detail!("vector cache miss: {}", p.display());
            }
            use rayon::prelude::*;
            use std::io::IsTerminal;
            use std::sync::atomic::{AtomicUsize, Ordering};

            let total = records.len();
            crate::status!(
                "embedding {total} records (one-time, then cached; a large schema can take ~a minute)…"
            );
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
            let v: Vec<Vec<f32>> = std::thread::scope(|scope| {
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
                records
                    .par_iter()
                    .map(|r| {
                        let out = EMBEDDER.with(|cell| {
                            let mut slot = cell.borrow_mut();
                            let emb = slot.get_or_insert_with(|| default_embedder(model));
                            compress_matryoshka_vector(&emb.embed(&record_text(r)))
                        });
                        done.fetch_add(1, Ordering::Relaxed);
                        out
                    })
                    .collect()
            });
            if let Some(p) = cache_path.as_deref() {
                cache::store(p, &v);
                cache::prune(cache::max_files()); // evict least-recently-used
            }
            v
        }
    };

    // warm() calls with an empty query and limit 0 purely to populate the cache
    // above — there's nothing to rank, so skip the query embed + cosine pass.
    if query.is_empty() && limit == 0 {
        return Vec::new();
    }

    let query_vec = compress_matryoshka_vector(&embedder.embed(query));
    let mut hits: Vec<(f64, &SchemaRecord)> = records
        .iter()
        .zip(&vectors)
        .filter(|(r, _)| kind.is_none_or(|k| r.kind == k))
        .filter(|(r, _)| {
            parent.is_none_or(|p| {
                r.parent
                    .as_deref()
                    .is_some_and(|rp| rp.eq_ignore_ascii_case(p))
            })
        })
        .map(|(r, v)| (cosine_similarity(&query_vec, v) as f64, r))
        .collect();

    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits.truncate(limit);
    hits
}

/// The natural-language signal a semantic query matches against: the path plus
/// the human description and the type.
fn record_text(r: &SchemaRecord) -> String {
    let mut s = r.path.clone();
    if let Some(d) = &r.description {
        s.push_str(" — ");
        s.push_str(d);
    }
    if let Some(t) = &r.type_ref {
        s.push_str(" : ");
        s.push_str(t);
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
    let _ = search("", records, None, None, 0, model, refresh);
    records.len()
}
