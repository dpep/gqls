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
use embed::default_embedder;
use mrl::{compress_matryoshka_vector, cosine_similarity};

/// Embed the query and each record, rank by cosine. Returns `(score, record)`
/// pairs, best first — the caller formats them exactly like fuzzy hits, so
/// `--json` / `--ndjson` work identically in both modes.
pub fn search<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    kind: Option<Kind>,
    limit: usize,
    model: Option<&str>,
    refresh: bool,
) -> Vec<(f64, &'a SchemaRecord)> {
    let embedder = default_embedder(model);
    eprintln!("gqls: semantic search via {} embeddings", embedder.kind());

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
                cache::touch(p); // LRU: mark this schema as recently used
            }
            v
        }
        None => {
            use rayon::prelude::*;
            use std::io::IsTerminal;
            use std::sync::atomic::{AtomicUsize, Ordering};

            let total = records.len();
            eprintln!(
                "gqls: embedding {total} records (one-time, then cached; a large schema can take ~a minute)…"
            );
            let done = AtomicUsize::new(0);

            // Per-worker embedder/session: the session is behind a Mutex, so a
            // shared one would serialize the parallel pass. The model is a cached
            // file, so every worker resolves the same kind as the main embedder
            // above (no mixed onnx/hash vectors). Order-preserving `collect`, so
            // vectors stay index-aligned with `records`.
            let v: Vec<Vec<f32>> = std::thread::scope(|scope| {
                let show_progress = std::io::stderr().is_terminal() && total > 500;
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
                    .map_init(
                        || default_embedder(model),
                        |emb, r| {
                            let out = compress_matryoshka_vector(&emb.embed(&record_text(r)));
                            done.fetch_add(1, Ordering::Relaxed);
                            out
                        },
                    )
                    .collect()
            });
            if let Some(p) = cache_path.as_deref() {
                cache::store(p, &v);
                cache::prune(cache::max_files()); // evict least-recently-used
            }
            v
        }
    };

    let query_vec = compress_matryoshka_vector(&embedder.embed(query));
    let mut hits: Vec<(f64, &SchemaRecord)> = records
        .iter()
        .zip(&vectors)
        .filter(|(r, _)| kind.is_none_or(|k| r.kind == k))
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

/// Whether the schema's vectors are already cached (either embedder kind), so a
/// semantic search would be warm. Checked without loading a model.
pub fn is_cached(records: &[SchemaRecord], model: Option<&str>) -> bool {
    cache::exists(records, "onnx", model) || cache::exists(records, "hash", model)
}

/// Embed + cache every record's vector without running a real query — pre-warms
/// the cache so the first search is instant. Returns the record count.
pub fn warm(records: &[SchemaRecord], model: Option<&str>, refresh: bool) -> usize {
    let _ = search("", records, None, 0, model, refresh);
    records.len()
}
