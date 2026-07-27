//! Fuzzy scoring.
//!
//! This is a compact, dependency-free placeholder. The plan is to replace it
//! with `rq`'s engine at `~/code/lib/rust/rq/src/search/score.rs` — a real DP
//! subsequence aligner (`align()`), an additive/explainable set of `Feature`s,
//! and confidence-by-dominance over the runner-up. The record shape here
//! (`SchemaRecord`) is deliberately close to rq's `SymbolRow` so that swap is
//! mostly a matter of adapting the `kind`-weight table.

use crate::model::SchemaRecord;

/// Score `query` against a record, or `None` if it doesn't match at all.
/// Higher is better.
pub fn score(query: &str, rec: &SchemaRecord) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();

    // Match the leaf name (the common case: `email`, `user`) OR the qualified
    // path (`user.email`) — the path at a slight discount so a clean leaf match
    // wins. Either may match; don't short-circuit on the name.
    let name = subsequence(&q, &rec.name);
    let path = subsequence(&q, &rec.path).map(|p| p - 50);
    let mut best = match (name, path) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };

    // Exact / prefix bonuses on the leaf name.
    let name_lower = rec.name.to_lowercase();
    let q_str: String = q.iter().collect();
    if name_lower == q_str {
        best += 1000;
    } else if name_lower.starts_with(&q_str) {
        best += 400;
    }

    best += rec.kind.weight();
    Some(best)
}

/// Abbreviation-aware subsequence match: every char of `q` (already
/// lowercased) must appear in `text` in order. Reward matches at word
/// boundaries (`camelCase`, `_`, `.`, `:`, `/`) and contiguous runs.
/// Returns `None` unless all of `q` is consumed.
fn subsequence(q: &[char], text: &str) -> Option<i64> {
    if q.is_empty() {
        return Some(0);
    }
    let tb: Vec<char> = text.chars().collect();
    let mut qi = 0;
    let mut score = 0i64;
    let mut last: Option<usize> = None;

    for (ti, &c) in tb.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if c.to_ascii_lowercase() == q[qi] {
            let at_boundary = ti == 0
                || matches!(tb[ti - 1], '_' | '.' | ':' | '/')
                || (c.is_ascii_uppercase() && tb[ti - 1].is_ascii_lowercase());
            score += if at_boundary { 15 } else { 3 };
            if let Some(l) = last {
                if l + 1 == ti {
                    score += 5; // contiguity bonus
                }
            }
            last = Some(ti);
            qi += 1;
        }
    }

    if qi == q.len() {
        // Prefer shorter, tighter matches.
        Some(score - (tb.len() as i64) / 4)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn rec(name: &str, path: &str, kind: Kind) -> SchemaRecord {
        SchemaRecord {
            path: path.into(),
            name: name.into(),
            kind,
            parent: None,
            type_ref: None,
            args: vec![],
            description: None,
            deprecated: None,
            directives: vec![],
        }
    }

    #[test]
    fn exact_beats_prefix_beats_fuzzy() {
        let exact = score("user", &rec("user", "Query.user", Kind::Query)).unwrap();
        let prefix = score("use", &rec("user", "Query.user", Kind::Query)).unwrap();
        let fuzzy = score("usr", &rec("user", "Query.user", Kind::Query)).unwrap();
        assert!(exact > prefix && prefix > fuzzy);
    }

    #[test]
    fn abbreviation_matches_camelcase() {
        // `cu` should hit the `createUser` boundaries.
        assert!(score("cu", &rec("createUser", "Mutation.createUser", Kind::Mutation)).is_some());
    }

    #[test]
    fn non_match_is_none() {
        assert!(score("xyz", &rec("user", "Query.user", Kind::Query)).is_none());
    }

    #[test]
    fn qualified_path_query_matches() {
        assert!(score("user.email", &rec("email", "User.email", Kind::Field)).is_some());
    }
}
