//! The read path: score every record against the query, rank, cut the weak tail.

use crate::model::{Kind, SchemaRecord};

pub mod score;

pub struct Hit<'a> {
    pub record: &'a SchemaRecord,
    pub score: i64,
}

/// Drop hits scoring below this fraction of the top hit. The scorer's tiers
/// (exact ≈1000, prefix ≈700, subsequence ≤600, typo ≤260) make the ratio
/// meaningful: a strong match present means the long subsequence tail is
/// noise; with only weak matches, everything in the same tier survives.
const TAIL_CUTOFF: f64 = 0.4;

/// If `query` is `Type.field` and `Type` exactly names an enclosing type in
/// the schema (case-insensitive), return the qualifier — the caller then
/// hard-filters to that type's members instead of fuzzy-matching types that
/// merely share the prefix (`Company.employe` stays out of
/// `CompanyProfileAndIntent`). `None` falls back to plain fuzzy matching.
pub fn exact_parent<'q>(query: &'q str, records: &[SchemaRecord]) -> Option<&'q str> {
    let (_, Some(qualifier)) = score::parse_qualified(query) else {
        return None;
    };
    records
        .iter()
        .filter_map(|r| r.parent.as_deref())
        .any(|p| p.eq_ignore_ascii_case(qualifier))
        .then_some(qualifier)
}

/// Fuzzy-search `records` for `query`, optionally restricted to one `kind`
/// and/or one enclosing `parent` type. Returns every hit above the quality
/// cutoff, best first — callers truncate to their own limit, so the length is
/// the true match count.
pub fn search<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    kind: Option<Kind>,
    parent: Option<&str>,
) -> Vec<Hit<'a>> {
    let mut hits: Vec<Hit> = records
        .iter()
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        .filter(|r| {
            parent.is_none_or(|p| {
                r.parent
                    .as_deref()
                    .is_some_and(|rp| rp.eq_ignore_ascii_case(p))
            })
        })
        .filter_map(|r| score::score(query, r).map(|score| Hit { record: r, score }))
        .collect();

    // highest score first; break ties toward the shorter path (the more
    // "central" definition — `User` before `AdminUserAuditLogEntry`).
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.record.path.len().cmp(&b.record.path.len()))
    });
    if let Some(top) = hits.first().map(|h| h.score) {
        let floor = (top as f64 * TAIL_CUTOFF) as i64;
        hits.retain(|h| h.score >= floor);
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, parent: Option<&str>, kind: Kind) -> SchemaRecord {
        let path = match parent {
            Some(p) => format!("{p}.{name}"),
            None => name.to_string(),
        };
        SchemaRecord {
            path,
            name: name.into(),
            kind,
            parent: parent.map(Into::into),
            type_ref: None,
            args: vec![],
            description: None,
            deprecated: None,
            directives: vec![],
        }
    }

    #[test]
    fn exact_parent_requires_a_real_type() {
        let records = vec![
            rec("employees", Some("Company"), Kind::Field),
            rec("name", Some("CompanyProfile"), Kind::Field),
        ];
        // exact type name (any case) → hard filter
        assert_eq!(exact_parent("Company.employe", &records), Some("Company"));
        assert_eq!(exact_parent("company.employe", &records), Some("company"));
        // a mere prefix of a type, or an unqualified query → no filter
        assert_eq!(exact_parent("Comp.employe", &records), None);
        assert_eq!(exact_parent("employe", &records), None);
    }

    #[test]
    fn parent_filter_excludes_other_types() {
        let records = vec![
            rec("employees", Some("Company"), Kind::Field),
            rec("employees", Some("CompanyProfile"), Kind::Field),
            rec("employer", Some("CompanyMemberStats"), Kind::Field),
        ];
        let hits = search("Company.employe", &records, None, Some("Company"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.path, "Company.employees");
    }

    #[test]
    fn weak_tail_is_cut_when_a_strong_match_exists() {
        let records = vec![
            rec("user", Some("Query"), Kind::Query),
            rec("userProfile", Some("Query"), Kind::Query),
            // matches `user` only as a scattered subsequence
            rec("uzszezr", Some("Query"), Kind::Query),
        ];
        let paths: Vec<&str> = search("user", &records, None, None)
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        assert_eq!(paths, ["Query.user", "Query.userProfile"]);
    }

    #[test]
    fn weak_matches_survive_when_nothing_stronger_exists() {
        let records = vec![rec("uzszezr", Some("Query"), Kind::Query)];
        assert_eq!(search("user", &records, None, None).len(), 1);
    }

    #[test]
    fn search_returns_all_hits_above_the_cutoff() {
        // no internal truncation — the caller applies its own limit
        let records: Vec<SchemaRecord> = (0..50)
            .map(|i| rec(&format!("user{i}"), Some("Query"), Kind::Query))
            .collect();
        assert_eq!(search("user", &records, None, None).len(), 50);
    }
}
