//! The read path: score every record against the query, rank, truncate.

use crate::model::{Kind, SchemaRecord};

pub mod score;

pub struct Hit<'a> {
    pub record: &'a SchemaRecord,
    pub score: i64,
}

/// Fuzzy-search `records` for `query`, optionally restricted to one `kind`.
pub fn search<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    kind: Option<Kind>,
    limit: usize,
) -> Vec<Hit<'a>> {
    let mut hits: Vec<Hit> = records
        .iter()
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        .filter_map(|r| score::score(query, r).map(|score| Hit { record: r, score }))
        .collect();

    // highest score first; break ties toward the shorter path (the more
    // "central" definition — `User` before `AdminUserAuditLogEntry`).
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.record.path.len().cmp(&b.record.path.len()))
    });
    hits.truncate(limit);
    hits
}
