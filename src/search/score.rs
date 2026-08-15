//! Fuzzy scoring — a DP subsequence aligner ported from `rq`
//! (`~/code/lib/rust/rq/src/search/score.rs`), adapted to `SchemaRecord`.
//!
//! Match quality dominates (exact > prefix > abbreviation), with a small kind
//! weight and a qualifier (`Type.field`) parent boost layered on. The aligner
//! is a real dynamic program — it rewards word-boundary (camelCase / `_`) and
//! contiguous matches, penalizes gaps, and only spans *adjacent* words — so
//! abbreviations like `refproc → RefundProcessor` rank the way a human reads.

use crate::model::SchemaRecord;

/// Score `query` against a record, best-is-higher, or `None` if it doesn't
/// match at all (not even as a subsequence).
pub(crate) fn score(query: &str, rec: &SchemaRecord) -> Option<i64> {
    // A qualified query (`Type.field`) names an enclosing type: match the leaf
    // against the name and reward a matching parent below.
    let (leaf, qualifier) = parse_qualified(query);
    let q = leaf.to_ascii_lowercase();
    let name_lower = rec.name.to_ascii_lowercase();

    let mut total = 0.0f64;

    // Match quality on the leaf name — the dominant term.
    let name_matched = if name_lower == q {
        total += 1000.0;
        true
    } else if name_lower.starts_with(&q) {
        let tail = rec.name.chars().count().saturating_sub(q.chars().count());
        total += 700.0 - (tail as f64).min(100.0);
        true
    } else if let Some(s) = subsequence_score(&q, &rec.name) {
        total += s.min(600.0);
        true
    } else if let Some(d) = typo_distance(&q, &name_lower) {
        // a transposition / single typo (`usre` -> `User`) isn't a clean
        // subsequence; match it, but rank below a real match, penalized by
        // edit distance.
        total += (260.0 - 70.0 * d as f64).max(80.0);
        true
    } else {
        false
    };

    // No name match: fall back to the qualified path (`user.email` vs
    // `User.email`) — a weaker signal, and required to match at all.
    if !name_matched {
        let s = subsequence_score(&query.to_ascii_lowercase(), &rec.path)?;
        total += (s * 0.5).min(300.0);
    }

    // Qualifier boost — the user named the enclosing type (`Repository.name`);
    // reward the field whose parent is that type.
    if let Some(qual) = qualifier {
        if let Some(b) = parent_boost(qual, rec.parent.as_deref()) {
            total += b;
        }
    }

    // Kind weight — roots and named types outrank leaf args (a tiebreaker).
    total += rec.kind.weight() as f64;

    Some(total.round() as i64)
}

/// Words too common to narrow a phrase: they subsequence-match nearly every
/// name, so they'd lift every record's word count by one and tell us nothing.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "for", "to", "in", "on", "at", "and", "or", "by", "with", "is", "that",
    "from", "as", "into",
];

/// Split a phrase query into the words worth matching on their own, or empty
/// when the query is a single word (the caller then scores it whole). Stopwords
/// and lone characters are dropped — unless that would leave nothing, in which
/// case the raw words stand.
pub(crate) fn phrase_tokens(query: &str) -> Vec<&str> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.len() < 2 {
        return Vec::new();
    }
    let kept: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| w.chars().count() > 1 && !STOPWORDS.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    if kept.is_empty() {
        words
    } else {
        kept
    }
}

/// Score a phrase against a record word by word: how many words matched, and
/// their summed quality. `None` when no word matches. Callers rank on the count
/// first — a name covering the whole phrase beats one echoing a single word.
pub(crate) fn score_phrase(tokens: &[&str], rec: &SchemaRecord) -> Option<(usize, i64)> {
    let mut matched = 0;
    let mut total = 0;
    for token in tokens {
        if let Some(s) = score(token, rec) {
            matched += 1;
            total += s;
        }
    }
    (matched > 0).then_some((matched, total))
}

/// Split a query into its leaf name and the optional enclosing type typed
/// before the last `.`: `User.email` → (`email`, `Some("User")`), a plain
/// `user` → (`user`, `None`). A leading/trailing `.` is an ordinary query.
pub(crate) fn parse_qualified(query: &str) -> (&str, Option<&str>) {
    match query.rfind('.') {
        Some(i) if i > 0 && i + 1 < query.len() => (&query[i + 1..], Some(&query[..i])),
        _ => (query, None),
    }
}

/// Boost a field whose enclosing type matches the query's qualifier. GraphQL
/// parents are a single type name, so an exact (case-insensitive) match is the
/// strong signal; a prefix (`repo` → `Repository`) a weaker one.
fn parent_boost(qualifier: &str, parent: Option<&str>) -> Option<f64> {
    let parent = parent?;
    if parent.eq_ignore_ascii_case(qualifier) {
        Some(300.0)
    } else if parent
        .to_ascii_lowercase()
        .starts_with(&qualifier.to_ascii_lowercase())
    {
        Some(150.0)
    } else {
        None
    }
}

/// Score `query` as a subsequence of `name` (the best alignment's score), or
/// `None` if it isn't a subsequence.
fn subsequence_score(query: &str, name: &str) -> Option<f64> {
    align(query, name).map(|a| a.score)
}

/// Edit distance between the (lowercased) query and name for the typo tier, or
/// `None` if it exceeds a small budget. Catches transposed/typo'd queries
/// (`usre` → `user`) that aren't a clean subsequence. Skipped for very short
/// queries, where a tiny edit distance would match almost anything.
pub(crate) fn typo_distance(q: &str, name_lower: &str) -> Option<usize> {
    let a: Vec<char> = q.chars().collect();
    if a.len() < 3 {
        return None;
    }
    let b: Vec<char> = name_lower.chars().collect();
    let max = if a.len() <= 5 { 1 } else { 2 };
    osa_within(&a, &b, max)
}

/// Bounded Optimal String Alignment distance (Levenshtein plus adjacent
/// transpositions): `None` if it exceeds `max`. Full matrix — names are short.
/// Inputs are already lowercased, so char equality suffices.
fn osa_within(a: &[char], b: &[char], max: usize) -> Option<usize> {
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return None;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1); // adjacent transposition
            }
            d[i][j] = v;
        }
    }
    (d[n][m] <= max).then_some(d[n][m])
}

// --- the aligner, ported verbatim from rq ---

/// Largest gap (chars skipped) allowed between two matched query chars that land
/// mid-word (not at a word boundary). Boundary jumps are how abbreviations work
/// and stay unlimited; off-boundary we tolerate a couple of skipped chars.
const MAX_NONBOUNDARY_GAP: usize = 2;

/// Penalty per skipped char between two matched chars.
const GAP_PENALTY: f64 = 3.0;

struct Alignment {
    score: f64,
}

/// Find the best alignment of `query` as a subsequence of `name`, maximizing
/// boundary and contiguous matches while penalizing gaps. `None` if `query`
/// isn't a subsequence. Query separators are ignored, so snake_case matches
/// CamelCase (`widget_controller → WidgetsController`).
fn align(query: &str, name: &str) -> Option<Alignment> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if q.is_empty() {
        return None;
    }
    // cheap gate: reject non-subsequences with one linear scan before the DP
    let mut qi = 0;
    for c in name.chars() {
        if qi < q.len() && c.to_ascii_lowercase() == q[qi] {
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    let chars: Vec<char> = name.chars().collect();
    let n = chars.len();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let boundary = boundaries(&chars);
    let mut bnd_prefix = vec![0usize; n + 1];
    for i in 0..n {
        bnd_prefix[i + 1] = bnd_prefix[i] + boundary[i] as usize;
    }

    // table[qi][i] = best (score, backpointer) for aligning q[0..=qi] with q[qi]
    // landing on name position `i`.
    let mut table: Vec<Vec<Option<(f64, usize)>>> = vec![vec![None; n]; q.len()];

    for (i, &c) in lower.iter().enumerate() {
        if c == q[0] {
            let mut s = 10.0;
            if boundary[i] {
                s += 15.0;
            }
            if i == 0 {
                s += 20.0;
            }
            table[0][i] = Some((s, i));
        }
    }

    for qi in 1..q.len() {
        for i in qi..n {
            if lower[i] != q[qi] {
                continue;
            }
            let base = 10.0 + if boundary[i] { 15.0 } else { 0.0 };
            let j_start = if boundary[i] {
                qi - 1
            } else {
                (qi - 1).max(i.saturating_sub(MAX_NONBOUNDARY_GAP + 1))
            };
            let mut best: Option<(f64, usize)> = None;
            let prev_row = &table[qi - 1];
            for (j, cell) in prev_row.iter().enumerate().take(i).skip(j_start) {
                let Some((pscore, _)) = cell else {
                    continue;
                };
                let trans = if j + 1 == i {
                    10.0
                } else {
                    let gap = i - j - 1;
                    let crossed_word = bnd_prefix[i] - bnd_prefix[j + 1] > 0;
                    if boundary[i] {
                        if crossed_word {
                            continue;
                        }
                    } else if gap > MAX_NONBOUNDARY_GAP || crossed_word {
                        continue;
                    }
                    -(gap as f64) * GAP_PENALTY
                };
                let cand = pscore + trans;
                if best.is_none_or(|(b, _)| cand > b) {
                    best = Some((cand, j));
                }
            }
            if let Some((bscore, j)) = best {
                table[qi][i] = Some((bscore + base, j));
            }
        }
    }

    let last = q.len() - 1;
    let score = (0..n)
        .filter_map(|i| table[last][i].map(|(s, _)| s))
        .max_by(|a, b| a.total_cmp(b))?;
    Some(Alignment {
        score: score.max(0.0),
    })
}

/// Mark word-boundary positions: index 0, anything after `_`/non-alphanumeric,
/// and camelCase humps (lower→Upper, and the last cap of an ACRONYMWord run).
pub(crate) fn boundaries(chars: &[char]) -> Vec<bool> {
    let mut out = vec![false; chars.len()];
    for i in 0..chars.len() {
        let c = chars[i];
        out[i] = if i == 0 {
            true
        } else {
            let prev = chars[i - 1];
            !prev.is_alphanumeric()
                || (c.is_uppercase() && prev.is_lowercase())
                || (c.is_uppercase()
                    && prev.is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase()))
        };
    }
    out
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
            parent: path.rsplit_once('.').map(|(p, _)| p.to_string()),
            type_ref: None,
            args: vec![],
            description: None,
            deprecated: None,
            directives: vec![],
            possible_types: vec![],
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
        assert!(score(
            "cu",
            &rec("createUser", "Mutation.createUser", Kind::Mutation)
        )
        .is_some());
        // real abbreviation the DP aligner handles well
        assert!(score(
            "refproc",
            &rec("refundProcessor", "T.refundProcessor", Kind::Field)
        )
        .is_some());
    }

    #[test]
    fn non_match_is_none() {
        assert!(score("xyz", &rec("user", "Query.user", Kind::Query)).is_none());
    }

    #[test]
    fn qualified_path_query_matches_and_boosts_the_right_parent() {
        // `User.email` — leaf `email` matches; both records share the leaf, but
        // the `User` qualifier must float the User field above the Account one.
        let user = score("user.email", &rec("email", "User.email", Kind::Field)).unwrap();
        let account = score("user.email", &rec("email", "Account.email", Kind::Field)).unwrap();
        assert!(user > account, "user {user} > account {account}");
    }

    #[test]
    fn transposition_typo_still_matches_below_a_clean_hit() {
        // `usre` (transposed `user`) is not a subsequence of `User`, but a single
        // adjacent transposition should still match — ranked below a clean match.
        let clean = score("user", &rec("User", "Query.user", Kind::Object)).unwrap();
        let typo = score("usre", &rec("User", "Query.user", Kind::Object)).unwrap();
        assert!(clean > typo, "clean {clean} > typo {typo}");
        // nonsense still doesn't match
        assert!(score("xqzw", &rec("User", "Query.user", Kind::Object)).is_none());
    }

    #[test]
    fn phrase_tokens_drops_noise_words_but_never_everything() {
        assert_eq!(
            phrase_tokens("cancel a subscription"),
            ["cancel", "subscription"]
        );
        // a lone word isn't a phrase — scored whole
        assert!(phrase_tokens("cancelSubscription").is_empty());
        // nothing survives the filter, so the raw words stand
        assert_eq!(phrase_tokens("of the"), ["of", "the"]);
    }

    #[test]
    fn phrase_scoring_counts_the_words_a_record_matches() {
        let both = score_phrase(
            &["cancel", "subscription"],
            &rec(
                "cancelSubscription",
                "Mutation.cancelSubscription",
                Kind::Mutation,
            ),
        );
        let one = score_phrase(
            &["cancel", "subscription"],
            &rec("subscriptionPlan", "T.subscriptionPlan", Kind::Field),
        );
        assert_eq!(both.unwrap().0, 2);
        assert_eq!(one.unwrap().0, 1);
        assert!(
            score_phrase(&["cancel", "subscription"], &rec("id", "T.id", Kind::Field)).is_none()
        );
    }

    #[test]
    fn adjacent_word_rule_rejects_scatter() {
        // skipping a whole middle word isn't a match
        assert!(score(
            "rndsvc",
            &rec("RefundProcessingService", "T.x", Kind::Object)
        )
        .is_none());
        // adjacent-word abbreviation still matches
        assert!(score(
            "refprocsvc",
            &rec("RefundProcessingService", "T.x", Kind::Object)
        )
        .is_some());
    }
}
