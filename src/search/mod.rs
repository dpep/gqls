//! The read path: score every record against the query, rank, cut the weak tail.

use crate::model::{Kind, SchemaRecord};

pub mod glob;
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

/// Whether some hit *names* the query's leaf: equal to it, or containing it
/// whole at a camelCase/underscore word boundary (`name` → `lastName`,
/// `User.name` → `User.fullName`). That's the scorer's strongest tier short
/// of exact — the signal that the user typed a word that really exists, so
/// meaning-based ranking would only append lookalike filler below it.
/// (GraphQL names are ASCII by spec, so byte and char indices agree.)
pub fn named_hit(query: &str, hits: &[Hit]) -> bool {
    let (leaf, _) = score::parse_qualified(query);
    let leaf = leaf.to_ascii_lowercase();
    if leaf.is_empty() {
        return false;
    }
    hits.iter().any(|h| {
        let lower = h.record.name.to_ascii_lowercase();
        let chars: Vec<char> = h.record.name.chars().collect();
        let boundary = score::boundaries(&chars);
        lower
            .match_indices(&leaf)
            .any(|(i, _)| boundary.get(i).copied().unwrap_or(false))
    })
}

/// How closely a query names a record, for the commands that act on one.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum NameMatch {
    /// The query is the record's name — and its type, where qualified.
    Exact,
    /// The same, once a small misspelling is corrected (`usre` → `user`).
    /// Worth telling the user about: they typed something else.
    Corrected,
}

/// Whether the query *names* this record, rather than merely ranking it first:
/// the leaf is the record's name outright or a small misspelling of it, and a
/// `Type.` qualifier names its parent the same way.
///
/// Ranking is happy to answer "closest of what's here" — right for a search,
/// wrong for a command that acts on a single record. `gqls usr -e` drafting an
/// operation for whatever `usr` subsequence-matched is a paste-ready answer to
/// a question the user didn't ask; the candidate list and a re-run cost less
/// than that.
pub fn names_the_record(query: &str, record: &SchemaRecord) -> Option<NameMatch> {
    let (leaf, qualifier) = score::parse_qualified(query);
    let leaf = near_exact(leaf, &record.name)?;
    let qualifier = match qualifier {
        Some(q) => near_exact(q, record.parent.as_deref()?)?,
        None => NameMatch::Exact,
    };
    // Either half being a correction makes the whole answer one.
    match (leaf, qualifier) {
        (NameMatch::Exact, NameMatch::Exact) => Some(NameMatch::Exact),
        _ => Some(NameMatch::Corrected),
    }
}

/// Whether the query is this record's name character for character, casing
/// included — `Role` for the enum, not for `User.role`.
///
/// [`names_the_record`] is deliberately case-insensitive: `createuser -e` should
/// draft rather than lecture. But case is real evidence of intent when it's the
/// thing telling two records apart, and GraphQL convention makes it reliable —
/// types are capitalised, fields aren't. So `Role` picks the enum out of a
/// result set that also holds `User.role`, while `role` stays the ambiguous
/// search it reads as.
pub fn names_the_record_exactly(query: &str, record: &SchemaRecord) -> bool {
    let (leaf, qualifier) = score::parse_qualified(query);
    leaf == record.name
        && match qualifier {
            Some(q) => record.parent.as_deref() == Some(q),
            None => true,
        }
}

/// Case-insensitively equal, or within the scorer's typo budget of it.
fn near_exact(query: &str, name: &str) -> Option<NameMatch> {
    let (q, n) = (query.to_ascii_lowercase(), name.to_ascii_lowercase());
    if q == n {
        return Some(NameMatch::Exact);
    }
    score::typo_distance(&q, &n).map(|_| NameMatch::Corrected)
}

/// Which records a search may consider, independent of the query itself: a
/// kind, an enclosing type, a return type. Grouped so every search path takes
/// one argument, and so adding a filter doesn't ripple through six signatures.
#[derive(Default, Clone, Copy)]
pub struct Filters<'a> {
    /// Only this kind of entity (`-k`).
    pub kind: Option<Kind>,
    /// Only members of this enclosing type (a resolved `Type.field` qualifier).
    pub parent: Option<&'a str>,
    /// Only fields whose type is this, wrappers peeled (`--returns`). Wildcards
    /// allowed; a plain name is an exact, case-insensitive match.
    pub returns: Option<&'a str>,
}

impl Filters<'_> {
    /// Compile once, then test many records — this is where the `returns`
    /// pattern gets parsed, rather than per record.
    pub fn compile(&self) -> Predicate<'_> {
        Predicate {
            kind: self.kind,
            parent: self.parent,
            returns: self.returns.map(glob::Pattern::new),
        }
    }
}

/// A compiled [`Filters`], ready to test records.
pub struct Predicate<'a> {
    kind: Option<Kind>,
    parent: Option<&'a str>,
    returns: Option<glob::Pattern>,
}

impl Predicate<'_> {
    /// Whether `r` passes every filter. A record with no type never satisfies
    /// a `returns` filter — asking what returns a `User` is asking about
    /// fields, not about the types themselves.
    pub fn accepts(&self, r: &SchemaRecord) -> bool {
        self.kind.is_none_or(|k| r.kind == k)
            && self.parent.is_none_or(|p| {
                r.parent
                    .as_deref()
                    .is_some_and(|rp| rp.eq_ignore_ascii_case(p))
            })
            && self
                .returns
                .as_ref()
                .is_none_or(|pat| r.base_type().is_some_and(|t| pat.matches(t)))
    }
}

/// Search `records` for `query` within `filters`. A wildcard query enumerates
/// exact matches; anything else is fuzzy-scored and its weak tail cut. Returns
/// every surviving hit, best first — callers truncate to their own limit, so
/// the length is the true match count.
pub fn search<'a>(query: &str, records: &'a [SchemaRecord], filters: Filters<'_>) -> Vec<Hit<'a>> {
    let predicate = filters.compile();
    if glob::is_pattern(query) {
        return glob_search(query, records, &predicate);
    }
    fuzzy_search(query, records, &predicate)
}

/// Enumerate the records a wildcard pattern matches. A pattern containing `.`
/// matches the qualified path (`User.*`, `*.email`); a bare one matches the
/// leaf name (`get*`). Every match is exact by construction, so there's no
/// quality tail to cut and no fuzzy score to rank by — order by kind (roots
/// and types before leaves), then alphabetically, so listings read predictably.
fn glob_search<'a>(
    pattern: &str,
    records: &'a [SchemaRecord],
    predicate: &Predicate<'_>,
) -> Vec<Hit<'a>> {
    use rayon::prelude::*;
    // Parsed once, then tested against every record.
    let pattern = glob::Pattern::new(pattern);
    let against_path = pattern.targets_path();
    let mut hits: Vec<Hit> = records
        .par_iter()
        .filter(|r| predicate.accepts(r))
        .filter(|r| {
            let text = if against_path { &r.path } else { &r.name };
            pattern.matches(text)
        })
        .map(|r| Hit {
            record: r,
            score: r.kind.weight(),
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.record.path.cmp(&b.record.path))
    });
    hits
}

fn fuzzy_search<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    predicate: &Predicate<'_>,
) -> Vec<Hit<'a>> {
    use rayon::prelude::*;
    // A multi-word query is matched word by word: no single name contains
    // "cancel a subscription" as one subsequence, so scoring it whole is a
    // guaranteed zero. Each word scores independently and the counts decide.
    let tokens = score::phrase_tokens(query);
    // Records score independently, so scan them in parallel — the win shows
    // on large schemas (tens of thousands of records), and rayon's overhead
    // is microseconds on small ones.
    let scored: Vec<(usize, Hit)> = records
        .par_iter()
        .filter(|r| predicate.accepts(r))
        .filter_map(|r| {
            let (matched, score) = if tokens.is_empty() {
                (1, score::score(query, r)?)
            } else {
                score::score_phrase(&tokens, r)?
            };
            Some((matched, Hit { record: r, score }))
        })
        .collect();

    // Intersect where the schema allows it, union where it doesn't: keep only
    // the records matching the most words of the phrase. `cancelSubscription`
    // covers both words of "cancel a subscription" and buries the many records
    // that merely echo one of them; when nothing covers both, the single-word
    // matches are all there is and they all stand. (Single-word queries are one
    // uniform group, so this is a no-op for them.)
    let best = scored.iter().map(|(m, _)| *m).max().unwrap_or(0);
    let mut hits: Vec<Hit> = scored
        .into_iter()
        .filter(|(m, _)| *m == best)
        .map(|(_, hit)| hit)
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
        typed_rec(name, parent, kind, None)
    }

    fn typed_rec(
        name: &str,
        parent: Option<&str>,
        kind: Kind,
        type_ref: Option<&str>,
    ) -> SchemaRecord {
        let path = match parent {
            Some(p) => format!("{p}.{name}"),
            None => name.to_string(),
        };
        SchemaRecord {
            path,
            name: name.into(),
            kind,
            parent: parent.map(Into::into),
            type_ref: type_ref.map(Into::into),
            args: vec![],
            description: None,
            deprecated: None,
            directives: vec![],
            possible_types: vec![],
        }
    }

    #[test]
    fn a_record_is_named_by_its_own_name_or_a_typo_of_it() {
        let r = rec("createUser", Some("Mutation"), Kind::Mutation);
        let exact = Some(NameMatch::Exact);
        assert_eq!(names_the_record("createUser", &r), exact);
        assert_eq!(names_the_record("createuser", &r), exact);
        assert_eq!(names_the_record("Mutation.createUser", &r), exact);
        // one transposition is still the field the user meant — but they typed
        // something else, so the caller has to be able to say so
        assert_eq!(
            names_the_record("createUesr", &r),
            Some(NameMatch::Corrected)
        );
        // a subsequence, or a name that merely contains the query, is not
        assert_eq!(names_the_record("cu", &r), None);
        assert_eq!(names_the_record("user", &r), None);
    }

    #[test]
    fn a_qualifier_has_to_name_the_records_type() {
        let r = rec("email", Some("User"), Kind::Field);
        assert_eq!(names_the_record("User.email", &r), Some(NameMatch::Exact));
        // a misspelt type is a correction just as a misspelt field is
        assert_eq!(
            names_the_record("Usre.email", &r),
            Some(NameMatch::Corrected)
        );
        // right field name under the wrong type — not what was asked for
        assert_eq!(names_the_record("Company.email", &r), None);
        // a prefix of the type is ambiguous, however well it ranks
        assert_eq!(names_the_record("Us.email", &r), None);
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
        let hits = search(
            "Company.employe",
            &records,
            Filters {
                parent: Some("Company"),
                ..Default::default()
            },
        );
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
    fn named_hit_accepts_exact_and_boundary_words() {
        let records = vec![rec("name", Some("User"), Kind::Field)];
        let hits = search("User.name", &records, Default::default());
        assert!(named_hit("User.name", &hits));
        assert!(named_hit("user.NAME", &hits));

        // the leaf appearing whole at a word boundary also counts —
        // `User.name` against a schema with only lastName/fullName
        let variants = vec![
            rec("lastName", Some("User"), Kind::Field),
            rec("fullName", Some("User"), Kind::Field),
        ];
        let hits = search(
            "User.name",
            &variants,
            Filters {
                parent: Some("User"),
                ..Default::default()
            },
        );
        assert!(named_hit("User.name", &hits));
    }

    #[test]
    fn named_hit_rejects_mid_word_and_scattered_matches() {
        let records = vec![rec("accountant", Some("User"), Kind::Field)];
        // `count` sits mid-word in accountant — a fuzzy match, not a naming
        let hits = search("count", &records, Default::default());
        assert!(!hits.is_empty());
        assert!(!named_hit("count", &hits));
    }

    #[test]
    fn wildcard_enumerates_a_types_members() {
        let records = vec![
            rec("email", Some("User"), Kind::Field),
            rec("id", Some("User"), Kind::Field),
            rec("email", Some("UserProfile"), Kind::Field),
            rec("User", None, Kind::Object),
        ];
        let paths: Vec<&str> = search("User.*", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        // only User's members, alphabetical — not UserProfile's, not User itself
        assert_eq!(paths, ["User.email", "User.id"]);
    }

    #[test]
    fn brace_alternation_enumerates_each_branch() {
        let records = vec![
            rec("firstName", Some("User"), Kind::Field),
            rec("lastName", Some("User"), Kind::Field),
            rec("middleName", Some("User"), Kind::Field),
        ];
        let paths: Vec<&str> = search("User.{first,last}Name", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        assert_eq!(paths, ["User.firstName", "User.lastName"]);
    }

    #[test]
    fn wildcard_without_a_dot_matches_leaf_names() {
        let records = vec![
            rec("getUser", Some("Query"), Kind::Query),
            rec("getCompany", Some("Query"), Kind::Query),
            rec("forget", Some("User"), Kind::Field),
        ];
        let paths: Vec<&str> = search("get*", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        // anchored, so `forget` is out; roots outrank fields, then alphabetical
        assert_eq!(paths, ["Query.getCompany", "Query.getUser"]);
    }

    #[test]
    fn wildcard_keeps_every_match_regardless_of_kind_weight() {
        // the fuzzy tail cutoff would drop a field (weight 20) under a root
        // (60); enumeration must not lose members that way
        let records = vec![
            rec("user", Some("Query"), Kind::Query),
            rec("name", Some("User"), Kind::Field),
        ];
        assert_eq!(search("*", &records, Default::default()).len(), 2);
    }

    #[test]
    fn returns_filter_finds_fields_by_return_type() {
        let records = vec![
            // the case a name search can't reach: the field isn't called Company
            typed_rec("myEmployer", Some("Query"), Kind::Query, Some("Company")),
            typed_rec(
                "employees",
                Some("Company"),
                Kind::Field,
                Some("[Employee!]!"),
            ),
            typed_rec("name", Some("Company"), Kind::Field, Some("String!")),
            typed_rec("Company", None, Kind::Object, None),
        ];
        let paths: Vec<&str> = search(
            "*",
            &records,
            Filters {
                returns: Some("Company"),
                ..Default::default()
            },
        )
        .iter()
        .map(|h| h.record.path.as_str())
        .collect();
        // the field returning Company — not the Company type itself
        assert_eq!(paths, ["Query.myEmployer"]);

        // wrappers are peeled: [Employee!]! matches Employee
        let paths: Vec<&str> = search(
            "*",
            &records,
            Filters {
                returns: Some("Employee"),
                ..Default::default()
            },
        )
        .iter()
        .map(|h| h.record.path.as_str())
        .collect();
        assert_eq!(paths, ["Company.employees"]);
    }

    #[test]
    fn returns_filter_accepts_wildcards_and_composes() {
        let records = vec![
            typed_rec("a", Some("Mutation"), Kind::Mutation, Some("APayload!")),
            typed_rec("b", Some("Mutation"), Kind::Mutation, Some("BPayload!")),
            typed_rec("c", Some("Mutation"), Kind::Mutation, Some("String")),
            typed_rec("d", Some("Type"), Kind::Field, Some("APayload")),
        ];
        // wildcard return type, narrowed by kind
        let paths: Vec<&str> = search(
            "*",
            &records,
            Filters {
                kind: Some(Kind::Mutation),
                returns: Some("*Payload"),
                ..Default::default()
            },
        )
        .iter()
        .map(|h| h.record.path.as_str())
        .collect();
        assert_eq!(paths, ["Mutation.a", "Mutation.b"]);
    }

    #[test]
    fn phrase_query_matches_each_word_and_prefers_full_coverage() {
        let records = vec![
            rec("cancelSubscription", Some("Mutation"), Kind::Mutation),
            rec("subscriptionPlan", Some("Billing"), Kind::Field),
            rec("cancelOrder", Some("Mutation"), Kind::Mutation),
        ];
        // the phrase is no longer a hard zero, and the record covering both
        // words wins outright — the noise word "a" changes nothing
        for query in ["cancel a subscription", "cancel subscription"] {
            let paths: Vec<&str> = search(query, &records, Default::default())
                .iter()
                .map(|h| h.record.path.as_str())
                .collect();
            assert_eq!(paths, ["Mutation.cancelSubscription"], "query {query:?}");
        }
    }

    #[test]
    fn qualified_queries_stay_off_the_phrase_path() {
        // Only whitespace opens the phrase path, so `Type.field` never takes it
        // — and `User name` doesn't either: spaced_qualifier rewrites it to the
        // dotted form first. Both stay precise navigation, not prose.
        let records = vec![
            rec("name", Some("User"), Kind::Field),
            rec("username", Some("Account"), Kind::Field),
        ];
        assert!(score::phrase_tokens("User.name").is_empty());
        assert_eq!(
            spaced_qualifier("User name", &records).as_deref(),
            Some("User.name")
        );
        // scoring `User.name` whole keeps the qualifier boost: User's field wins
        let paths: Vec<&str> = search("User.name", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        assert_eq!(paths.first(), Some(&"User.name"));
    }

    #[test]
    fn phrase_query_falls_back_to_single_word_matches() {
        let records = vec![
            rec("subscriptionPlan", Some("Billing"), Kind::Field),
            rec("cancelOrder", Some("Mutation"), Kind::Mutation),
            rec("id", Some("Billing"), Kind::Field),
        ];
        // nothing covers both words, so every one-word match stands
        let mut paths: Vec<&str> = search("cancel subscription", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, ["Billing.subscriptionPlan", "Mutation.cancelOrder"]);
    }

    #[test]
    fn weak_tail_is_cut_when_a_strong_match_exists() {
        let records = vec![
            rec("user", Some("Query"), Kind::Query),
            rec("userProfile", Some("Query"), Kind::Query),
            // matches `user` only as a scattered subsequence
            rec("uzszezr", Some("Query"), Kind::Query),
        ];
        let paths: Vec<&str> = search("user", &records, Default::default())
            .iter()
            .map(|h| h.record.path.as_str())
            .collect();
        assert_eq!(paths, ["Query.user", "Query.userProfile"]);
    }

    #[test]
    fn weak_matches_survive_when_nothing_stronger_exists() {
        let records = vec![rec("uzszezr", Some("Query"), Kind::Query)];
        assert_eq!(search("user", &records, Default::default()).len(), 1);
    }

    #[test]
    fn search_returns_all_hits_above_the_cutoff() {
        // no internal truncation — the caller applies its own limit
        let records: Vec<SchemaRecord> = (0..50)
            .map(|i| rec(&format!("user{i}"), Some("Query"), Kind::Query))
            .collect();
        assert_eq!(search("user", &records, Default::default()).len(), 50);
    }
}
