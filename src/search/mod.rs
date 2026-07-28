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

/// Resolve a `Type.field` query's qualifier to a schema type: an exact
/// (case-insensitive) parent name, or failing that the unique closest
/// misspelling within the scorer's typo budget (`Compnay.employe` →
/// `Company`). Returns the schema's own spelling of the type — the caller
/// hard-filters to its members instead of fuzzy-matching types that merely
/// share the prefix (`Company.employe` stays out of
/// `CompanyProfileAndIntent`). `None` (unqualified, no type close enough, or
/// a distance tie) falls back to plain fuzzy matching.
pub fn parent_filter<'a>(query: &str, records: &'a [SchemaRecord]) -> Option<&'a str> {
    let (_, Some(qualifier)) = score::parse_qualified(query) else {
        return None;
    };
    let parents: std::collections::HashSet<&str> =
        records.iter().filter_map(|r| r.parent.as_deref()).collect();
    if let Some(exact) = parents.iter().find(|p| p.eq_ignore_ascii_case(qualifier)) {
        return Some(exact);
    }
    // Misspelling fallback: the unique closest type wins; a tie is ambiguous.
    let q = qualifier.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    for p in parents {
        let Some(d) = score::typo_distance(&q, &p.to_ascii_lowercase()) else {
            continue;
        };
        match best {
            Some((bd, _)) if d > bd => {}
            Some((bd, _)) if d == bd => tied = true,
            _ => {
                best = Some((d, p));
                tied = false;
            }
        }
    }
    match (best, tied) {
        (Some((_, p)), false) => Some(p),
        _ => None,
    }
}

/// Rewrite a two-word query whose first word exactly names a schema type into
/// the qualified form: `User name` → `User.name`. The space (vs the dot) is
/// kept as an intent signal by the caller — it means "around this", so the
/// semantic combine stays on where a dot-typed exact hit would skip it.
/// `None` for anything else (phrases, dots, no matching type).
pub fn spaced_qualifier(query: &str, records: &[SchemaRecord]) -> Option<String> {
    if query.contains('.') {
        return None;
    }
    let mut words = query.split_whitespace();
    let (Some(first), Some(second), None) = (words.next(), words.next(), words.next()) else {
        return None;
    };
    records
        .iter()
        .filter_map(|r| r.parent.as_deref())
        .any(|p| p.eq_ignore_ascii_case(first))
        .then(|| format!("{first}.{second}"))
}

/// Whether any hit's name equals the query's leaf exactly (case-insensitive)
/// — the signal that the query *named* a specific entity rather than described
/// one, so meaning-based ranking would only append lookalike filler.
pub fn has_exact(query: &str, hits: &[Hit]) -> bool {
    let (leaf, _) = score::parse_qualified(query);
    hits.iter()
        .any(|h| h.record.name.eq_ignore_ascii_case(leaf))
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
    use rayon::prelude::*;
    // Records score independently, so scan them in parallel — the win shows
    // on large schemas (tens of thousands of records), and rayon's overhead
    // is microseconds on small ones.
    let mut hits: Vec<Hit> = records
        .par_iter()
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
    fn parent_filter_resolves_a_real_type() {
        let records = vec![
            rec("employees", Some("Company"), Kind::Field),
            rec("name", Some("CompanyProfile"), Kind::Field),
        ];
        // exact type name, any case — returns the schema's spelling
        assert_eq!(parent_filter("Company.employe", &records), Some("Company"));
        assert_eq!(parent_filter("company.employe", &records), Some("Company"));
        // a mere prefix of a type, or an unqualified query → no filter
        assert_eq!(parent_filter("Comp.employe", &records), None);
        assert_eq!(parent_filter("employe", &records), None);
    }

    #[test]
    fn parent_filter_snaps_a_misspelled_type_to_the_closest() {
        let records = vec![
            rec("employees", Some("Company"), Kind::Field),
            rec("name", Some("CompanyProfile"), Kind::Field),
        ];
        // transposition: Compnay → Company (CompanyProfile is out of budget)
        assert_eq!(parent_filter("Compnay.employe", &records), Some("Company"));
        // nothing close enough → no filter
        assert_eq!(parent_filter("Zebra.employe", &records), None);
    }

    #[test]
    fn parent_filter_declines_an_ambiguous_misspelling() {
        let records = vec![
            rec("id", Some("Vser"), Kind::Field),
            rec("id", Some("Usor"), Kind::Field),
        ];
        // `User` is distance 1 from both — ambiguous, so no filter
        assert_eq!(parent_filter("User.id", &records), None);
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
    fn spaced_qualifier_rewrites_type_plus_word() {
        let records = vec![
            rec("name", Some("User"), Kind::Field),
            rec("lastName", Some("User"), Kind::Field),
        ];
        assert_eq!(
            spaced_qualifier("User name", &records).as_deref(),
            Some("User.name")
        );
        assert_eq!(
            spaced_qualifier("user name", &records).as_deref(),
            Some("user.name")
        );
        // phrases, dots, and non-type first words stay untouched
        assert_eq!(spaced_qualifier("cancel a subscription", &records), None);
        assert_eq!(spaced_qualifier("User.name", &records), None);
        assert_eq!(spaced_qualifier("employee summary", &records), None);
    }

    #[test]
    fn has_exact_matches_the_leaf_name_only() {
        let records = vec![
            rec("name", Some("User"), Kind::Field),
            rec("username", Some("Query"), Kind::Query),
        ];
        let hits = search("User.name", &records, None, None);
        assert!(has_exact("User.name", &hits));
        assert!(has_exact("user.NAME", &hits));
        let near = search("User.nam", &records, None, None);
        assert!(!has_exact("User.nam", &near));
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
