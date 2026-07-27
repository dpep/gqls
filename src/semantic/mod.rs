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
) -> Vec<(f64, &'a SchemaRecord)> {
    let embedder = default_embedder(model);
    eprintln!("gqls: semantic search via {} embeddings", embedder.kind());

    // Per-record vectors are the expensive part; cache them by schema+embedder
    // so only the query is embedded on a warm run. Index-aligned with `records`.
    let cache_path = cache::path(records, embedder.kind(), model);
    let vectors = cache_path
        .as_deref()
        .and_then(|p| cache::load(p, records.len()))
        .unwrap_or_else(|| {
            eprintln!("gqls: embedding {} records (caching)…", records.len());
            let v: Vec<Vec<f32>> = records
                .iter()
                .map(|r| compress_matryoshka_vector(&embedder.embed(&record_text(r))))
                .collect();
            if let Some(p) = cache_path.as_deref() {
                cache::store(p, &v);
            }
            v
        });

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
