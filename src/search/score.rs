//! Fuzzy scoring — a DP subsequence aligner ported from `rq`
//! (`~/code/lib/rust/rq/src/search/score.rs`), adapted to `SchemaRecord`.
//!
//! A score is one number on one scale: **the fraction of a perfect match this
//! is**, where perfect means the query *is* the name. 1.0 is exact, ~0.95 a
//! short prefix, ~0.7 a clean word inside a longer name, ~0.5 a correction —
//! and those mean the same thing whatever the query's length, the name's
//! length, or which branch produced them. That is what lets the weak-tail cut
//! compare two matches as a ratio, and what keeps a 7-char query's 88% match
//! from scoring an order of magnitude under an 8-char query's 99% one.
//!
//! Two things make the number: how cleanly the query's characters sit in the
//! name ([`align`], divided by [`perfect`]), and how much of the name it
//! accounts for ([`share`]). Neither is enough alone — `dispute` sits perfectly
//! inside `in_app_disputes` and is half of it, while `uesr` sits just as
//! perfectly inside `closingIssuesReferences` and is a sixth of it.
//!
//! The aligner underneath is a real dynamic program — it rewards word-boundary
//! (camelCase / `_`) and contiguous matches, penalizes gaps, and only spans
//! *adjacent* words — so abbreviations like `refproc → RefundProcessor` rank
//! the way a human reads.
//!
//! Kind weight is not in the score. A root field and the type it returns are
//! equally good answers to the same query; which one you want is a tiebreak,
//! and the caller sorts on it as one.

use crate::model::SchemaRecord;

/// How a query is matched against a record. Names and paths are the ordinary
/// pass; arguments are the fallback the caller runs only when that one came
/// back empty.
pub(crate) type Scorer = fn(&str, &SchemaRecord) -> Option<Match>;

/// What a scorer found. Ranking and cutting ask different questions, so the
/// answer carries both.
#[derive(Clone, Copy)]
pub(crate) struct Match {
    /// The query *is* the record's name. Quality 1.0 says the same thing, but
    /// the cut acts on this rather than on the number: it's a fact about the
    /// two strings, not a threshold a future band could drift into.
    pub(crate) exact: bool,
    /// The query matched the record's own name, rather than its path or an
    /// argument — both of which are somebody else's name. Read for the same
    /// reason as `exact`: the argument pass asks whether anything was *named*
    /// like the query, and no constant sits under every name match, so a weak
    /// one can score below a path accident on a long path.
    pub(crate) named: bool,
    /// What the tail cut compares: the quality, on the 0..[`SCALE`] scale. The
    /// qualifier boost is left out because it can't tell two candidates apart —
    /// every member of the named type is handed the same one — so including it
    /// would make a more precise query cut its tail *less*.
    pub(crate) merit: i64,
    /// Ranking order: `merit` plus that boost, which is what floats the right
    /// type's fields when the qualifier named no type to filter by.
    pub(crate) score: i64,
}

/// A perfect match: the query is the name.
const EXACT: f64 = 1.0;

/// The floor of the band a match that *starts* the name lands in. Below it sits
/// every match that starts somewhere else, so a prefix outranks a containment
/// however cleanly the containment aligned — the separation the old prefix tier
/// had, kept, because merging the two is what buried `Package` under
/// `deletePackageVersion`.
const ANCHOR: f64 = 0.80;

/// How much of a match's quality is *how much of the name it accounts for*, the
/// rest being how cleanly it aligned.
const SHARE: f64 = 0.50;

/// The ceiling on a match the user had to be corrected into. Under [`ANCHOR`],
/// so no correction outranks a match that is really there, and above the weak
/// end of a containment, so `uesr` still finds `user` rather than the four
/// letters that happen to run together inside `closingIssuesReferences`.
const TYPO: f64 = 0.65;

/// What a match on the qualified path is worth against the same match on the
/// name — the last signal the name pass has, and the weakest. Under
/// `TAIL_CUTOFF` of the anchored band on purpose: the record's own name didn't
/// match, so the moment anything's did, this one is the tail.
const PATH: f64 = 0.25;

/// What naming the enclosing type exactly (`Repository.name`) is worth, and
/// what a mere prefix of it is. On the same scale as everything else: naming
/// the type is worth a third of naming the field.
const QUALIFIED: f64 = 0.30;
const QUALIFIED_PREFIX: f64 = 0.15;

/// A quality of 1.0, as the integer the rest of gqls reports and sorts on.
/// Large enough that the differences that matter — a character of name length,
/// a word of it — survive rounding.
const SCALE: f64 = 1000.0;

/// Score `query` against a record's name or qualified path, best-is-higher, or
/// `None` if neither matches (not even as a subsequence).
pub(crate) fn score(query: &str, rec: &SchemaRecord) -> Option<Match> {
    // A qualified query (`Type.field`) names an enclosing type: match the leaf
    // against the name and reward a matching parent below.
    let (leaf, qualifier) = parse_qualified(query);
    let q = leaf.to_ascii_lowercase();
    let name_lower = rec.name.to_ascii_lowercase();

    if name_lower == q {
        return Some(Match {
            exact: true,
            ..finish(EXACT, qualifier, rec)
        });
    }
    let quality = if let Some(m) = match_quality(&q, &rec.name, &name_lower) {
        // A match that starts the name lands in the band above every match that
        // doesn't, and takes the same measure of itself within it.
        match name_lower.starts_with(&q) {
            true => ANCHOR + (EXACT - ANCHOR) * m,
            false => ANCHOR * m,
        }
    } else if let Some(d) = typo_distance(&q, &name_lower) {
        // A transposition / single typo (`usre` -> `User`) isn't a subsequence
        // at all. It covers the whole name by construction, so what's left to
        // measure is how much of it survived the correction — an edit costs
        // less of a long name than of a short one, which is the point.
        TYPO * (1.0 - d as f64 / q.len().max(name_lower.len()) as f64)
    } else {
        // Nothing of the name matched, by either reading of it.
        return score_path(query, qualifier, rec);
    };

    Some(finish(quality, qualifier, rec))
}

/// The name pass's last resort: the query against the record's qualified path
/// (`user.email` vs `User.email`), measured like a name match and discounted
/// by [`PATH`] for not being one. The path is this record's name with another
/// record's in front of it, so matching it names neither.
fn score_path(query: &str, qualifier: Option<&str>, rec: &SchemaRecord) -> Option<Match> {
    let path_lower = rec.path.to_ascii_lowercase();
    let m = match_quality(&query.to_ascii_lowercase(), &rec.path, &path_lower)?;
    Some(Match {
        named: false,
        ..finish(PATH * m, qualifier, rec)
    })
}

/// How good a match `query` is for `text`, in [0, 1]: how cleanly it aligned,
/// times how much of `text` it accounts for. `None` if it isn't a subsequence.
/// `text_lower` is `text` lowercased — the aligner needs the original casing to
/// see camelCase humps, and the rest needs it gone.
fn match_quality(query: &str, text: &str, text_lower: &str) -> Option<f64> {
    let a = align(query, text)?;
    // Above 1.0 means nothing: a query spanning an interior word boundary
    // collects a bonus the perfect run has no boundary to earn.
    let fidelity = (a.score / perfect(a.len)).min(EXACT);
    Some(fidelity * (1.0 - SHARE + SHARE * share(query, text_lower, &a)))
}

/// How much of `text` the query accounts for, measured from where the match
/// begins: the characters it matched, plus those of every *further* place the
/// query occurs in full, over what is left of the text from that point.
///
/// Measuring from the match's start is the head-noun rule: what a name puts
/// *before* the match qualifies it, what it puts *after* means the match wasn't
/// what the name is about. `pokemon_v2_movelearnmethod` is a move-learn-method;
/// `move_learn_method_id` is an id. Count from the start of the name and the
/// second wins for being shorter, which on a schema where every name begins
/// `pokemon_v2_` is the whole table list in the wrong order.
///
/// Counting the *further* occurrences is the same rule seen from the other end:
/// `pokemon` is 78% of `pokemon_v2_pokemon` and 47% of `pokemon_v2_item`, and
/// crediting only the occurrence the aligner consumed makes the shared prefix
/// carry the answer.
fn share(query: &str, text_lower: &str, a: &Alignment) -> f64 {
    let from = text_lower
        .char_indices()
        .nth(a.start)
        .map_or(text_lower.len(), |(i, _)| i);
    let rest = &text_lower[from..];
    let len = rest.chars().count();
    if len == 0 {
        return 0.0;
    }
    let mut occurrences = 0;
    let mut scan = rest;
    while let Some(i) = scan.find(query) {
        occurrences += 1;
        scan = &scan[i + query.len()..];
    }
    ((a.len * occurrences.max(1)) as f64 / len as f64).min(1.0)
}

/// Score `query` against the record's *argument* names, or `None` if none of
/// them match — the fallback pass, so `followRenames` finds `Query.repository`.
///
/// An argument isn't a record of its own, so the field that *takes* it is the
/// answer — which is what you'd call anyway, and naming it then says what the
/// argument is for. It's a separate pass rather than a lower tier of [`score`]
/// because "only when nothing else matched" is a property of the whole result
/// set, not of any one record: a bad name match can score as low as you like,
/// so no constant sits below every one of them, and a single pass could only
/// approximate the rule. Scoring an exact argument above a weak name match
/// displaced the input objects actually named `input` on a Relay schema, where
/// hundreds of mutations take one.
pub(crate) fn score_arg(query: &str, rec: &SchemaRecord) -> Option<Match> {
    let (leaf, qualifier) = parse_qualified(query);
    let quality = best_arg_match(&leaf.to_ascii_lowercase(), rec)?;
    // Naming an argument exactly is not naming the record: the record is the
    // field that takes it, and both set-level rules are about records.
    Some(Match {
        named: false,
        ..finish(quality, qualifier, rec)
    })
}

/// Put a quality on the reported scale and add the boost a `Type.` qualifier
/// earns the field whose parent it names (`Repository.name`). Reports the
/// ordinary case — the query matched the name, but isn't it; a caller whose
/// match is something else says so at its own call site.
fn finish(quality: f64, qualifier: Option<&str>, rec: &SchemaRecord) -> Match {
    let boost = qualifier
        .and_then(|q| parent_boost(q, rec.parent.as_deref()))
        .unwrap_or(0.0);
    Match {
        exact: false,
        named: true,
        merit: (quality * SCALE).round() as i64,
        score: ((quality + boost) * SCALE).round() as i64,
    }
}

/// How well `query` matches any of `rec`'s argument names, or `None` for none.
/// An exact argument name outranks a subsequence of one; these qualities only
/// ever rank against each other, within the fallback pass.
fn best_arg_match(query: &str, rec: &SchemaRecord) -> Option<f64> {
    /// A subsequence of an argument name, against naming one outright.
    const ARG_SUBSEQUENCE: f64 = 0.5;
    rec.arg_types()
        .filter_map(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            match lower == query {
                true => Some(EXACT),
                false => match_quality(query, name, &lower).map(|m| ARG_SUBSEQUENCE * m),
            }
        })
        .fold(None, |best: Option<f64>, s| {
            Some(best.map_or(s, |b| b.max(s)))
        })
}

/// Words too common to narrow a phrase: they subsequence-match nearly every
/// name, so they'd lift every record's word count by one and tell us nothing.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "for", "to", "in", "on", "at", "and", "or", "by", "with", "is", "that",
    "from", "as", "into", "was", "were", "are", "be", "been", "has", "have", "had", "does", "do",
    "did", "can", "could", "should", "would", "will", "this", "these", "those", "it", "its", "my",
    "our", "their", "any", "some", "much", "many", "me", "we", "you", "i", "what", "when", "where",
    "who", "which", "whose", "why", "how", "still", "up", "about",
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

/// How much of a phrase a record covers, and how well.
pub(crate) struct Coverage {
    /// Words the record's own name (or path, or argument) matched. The bar a
    /// phrase has to clear is the best of these across the schema.
    pub(crate) named: usize,
    /// Those plus the words only its description carries.
    pub(crate) matched: usize,
    pub(crate) score: Match,
}

/// A word the description carries but the name doesn't. Worth counting and
/// barely worth scoring: it decides coverage — `Query.viewer` covers both
/// words of "current user" through "The currently authenticated user" — while
/// staying far below the weakest name match, so within one coverage tier a
/// name always wins.
const DESCRIPTION_WORD: i64 = 1;

/// Whether `word` appears whole in `text`, ignoring case. Prose, so the
/// boundaries are non-alphanumerics rather than camelCase humps; `current`
/// matches "currently", since English suffixes are how a description says the
/// same thing a name does.
fn described_by(word: &str, text: &str) -> bool {
    // Byte-wise and allocation-free: this runs per unmatched word per record,
    // so lowercasing a copy of every description was the whole added cost of
    // consulting them.
    let (word, text) = (word.as_bytes(), text.as_bytes());
    if word.is_empty() || word.len() > text.len() {
        return false;
    }
    (0..=text.len() - word.len())
        .filter(|&i| i == 0 || !text[i - 1].is_ascii_alphanumeric())
        .any(|i| text[i..i + word.len()].eq_ignore_ascii_case(word))
}

/// Score a phrase against a record word by word: how many words matched, and
/// their summed quality. `None` when no word matches. Callers rank on the count
/// first — a name covering the whole phrase beats one echoing a single word.
///
/// A word the name misses is looked for in the description, because that is
/// where a schema says in the user's vocabulary what its names say in its own
/// ("who is logged in" against `viewer`). Measured on GitHub's schema: of the
/// phrase queries whose answer the name can't reach, three quarters name it in
/// the description.
pub(crate) fn score_phrase(
    tokens: &[&str],
    rec: &SchemaRecord,
    scorer: Scorer,
) -> Option<Coverage> {
    let mut named = 0;
    let mut matched = 0;
    let mut sum = Match {
        exact: true,
        named: false,
        merit: 0,
        score: 0,
    };
    for token in tokens {
        if let Some(m) = scorer(token, rec) {
            named += 1;
            matched += 1;
            // Exact only if every word it matched was; named if any was.
            sum.exact &= m.exact;
            sum.named |= m.named;
            sum.merit += m.merit;
            sum.score += m.score;
        } else if rec
            .description
            .as_deref()
            .is_some_and(|d| described_by(token, d))
        {
            matched += 1;
            sum.exact = false;
            sum.merit += DESCRIPTION_WORD;
            sum.score += DESCRIPTION_WORD;
        }
    }
    (matched > 0).then_some(Coverage {
        named,
        matched,
        score: sum,
    })
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
        Some(QUALIFIED)
    } else if parent
        .to_ascii_lowercase()
        .starts_with(&qualifier.to_ascii_lowercase())
    {
        Some(QUALIFIED_PREFIX)
    } else {
        None
    }
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

/// What one matched query char is worth on its own.
const MATCH: f64 = 10.0;
/// Extra for landing on a word boundary — a camelCase hump or after a `_`.
const BOUNDARY: f64 = 15.0;
/// Extra for landing on the name's first char.
const START: f64 = 20.0;
/// Extra for following the previously matched char with no gap.
const CONTIGUOUS: f64 = 10.0;

struct Alignment {
    score: f64,
    /// The query's length *as the aligner saw it*, separators dropped — what
    /// [`perfect`] has to be measured against for the ratio to mean anything.
    len: usize,
    /// Where in the name the first query char landed, so [`share`] can measure
    /// what the match left unexplained rather than what preceded it.
    start: usize,
}

/// What an `n`-char query scores when it *is* the name: every char matched,
/// contiguously, from the first. Derived from the aligner's own constants so
/// the two can't drift.
fn perfect(n: usize) -> f64 {
    MATCH + BOUNDARY + START + (n.saturating_sub(1) as f64) * (MATCH + CONTIGUOUS)
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
            let mut s = MATCH;
            if boundary[i] {
                s += BOUNDARY;
            }
            if i == 0 {
                s += START;
            }
            table[0][i] = Some((s, i));
        }
    }

    for qi in 1..q.len() {
        for i in qi..n {
            if lower[i] != q[qi] {
                continue;
            }
            let base = MATCH + if boundary[i] { BOUNDARY } else { 0.0 };
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
                    CONTIGUOUS
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
    let (score, end) = (0..n)
        .filter_map(|i| table[last][i].map(|(s, _)| (s, i)))
        .max_by(|(a, _), (b, _)| a.total_cmp(b))?;
    // Walk the backpointers home for the first matched position.
    let mut start = end;
    for qi in (1..q.len()).rev() {
        start = table[qi][start].expect("on the winning path").1;
    }
    Some(Alignment {
        score: score.max(0.0),
        len: q.len(),
        start,
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
            arg_descriptions: Default::default(),
            description: None,
            deprecated: None,
            directives: vec![],
            default: None,
            possible_types: vec![],
        }
    }

    #[test]
    fn exact_beats_prefix_beats_fuzzy() {
        let exact = score("user", &rec("user", "Query.user", Kind::Query)).unwrap();
        let prefix = score("use", &rec("user", "Query.user", Kind::Query)).unwrap();
        let fuzzy = score("usr", &rec("user", "Query.user", Kind::Query)).unwrap();
        assert!(exact.score > prefix.score && prefix.score > fuzzy.score);
        // only the first is the top tier the tail cut recognises
        assert!(exact.exact);
        assert!(!prefix.exact && !fuzzy.exact);
    }

    /// A match's score as a fraction of the best possible for that query — the
    /// thing the whole scale is for.
    fn fraction(query: &str, name: &str) -> f64 {
        let got = score(query, &rec(name, &format!("T.{name}"), Kind::Field)).unwrap();
        let best = score(query, &rec(query, &format!("T.{query}"), Kind::Field)).unwrap();
        got.score as f64 / best.score as f64
    }

    #[test]
    fn the_same_match_is_worth_the_same_at_any_query_length() {
        // The same shape of match — the query as a whole word at a boundary,
        // one word of filler either side — at three query lengths. Under the
        // raw aligner score these came out 0.08, 0.16 and 0.30 of a perfect
        // match: the tier ceiling was "20 x query length" wearing a tier's
        // clothes, so a cut that compared them compared nothing.
        let spread = [
            fraction("cat", "in_app_cats"),
            fraction("dispute", "in_app_disputes"),
            fraction("reconciliation", "in_app_reconciliations"),
        ];
        let (lo, hi) = (
            spread.iter().cloned().fold(f64::MAX, f64::min),
            spread.iter().cloned().fold(0.0, f64::max),
        );
        // Some spread is real and stays: a containment can't earn the bonus for
        // starting the name, and that bonus is a bigger share of a short query.
        // Three letters buried in a name *are* weaker evidence than fourteen.
        assert!(
            hi / lo < 1.5,
            "same match, three lengths, spread {spread:?} — 3.6x apart is what \
             this replaced, and a ratio can't mean anything across that"
        );
        assert!(
            lo > 0.5,
            "the weakest of {spread:?} is under half a perfect match, so the \
             weak-tail cut would drop all three"
        );
    }

    #[test]
    fn a_shared_prefix_does_not_hand_the_answer_to_the_shortest_name() {
        // Every name on a Hasura schema starts `pokemon_v2_`, so the leading
        // occurrence says nothing and the tail-length tiebreak inverts: the
        // record the query names twice must beat three unrelated shorter ones.
        let wanted = score(
            "pokemon",
            &rec("pokemon_v2_pokemon", "query_root.x", Kind::Query),
        )
        .unwrap()
        .score;
        for other in ["pokemon_v2_item", "pokemon_v2_move", "pokemon_v2_berry"] {
            let got = score("pokemon", &rec(other, "query_root.y", Kind::Query))
                .unwrap()
                .score;
            assert!(
                got < wanted,
                "{other} scored {got}, pokemon_v2_pokemon {wanted}"
            );
        }
    }

    #[test]
    fn what_a_name_puts_after_the_match_counts_against_it() {
        // `movelearnmethod` is what `pokemon_v2_movelearnmethod` *is*, and only
        // the qualifier of what `move_learn_method_id` is. Measured from the
        // start of the name the shorter one wins for being shorter — which is
        // every table on a Hasura schema ranked under its own id columns.
        let table = score(
            "movelearnmethod",
            &rec("pokemon_v2_movelearnmethod", "query_root.x", Kind::Query),
        )
        .unwrap();
        let column = score(
            "movelearnmethod",
            &rec(
                "move_learn_method_id",
                "T.move_learn_method_id",
                Kind::Field,
            ),
        )
        .unwrap();
        assert!(
            table.score > column.score,
            "table {} column {}",
            table.score,
            column.score
        );
    }

    #[test]
    fn a_correction_outranks_letters_that_merely_run_together() {
        // `uesr` is one transposition from `user` and a clean four-char run
        // inside `closingIssuesReferences` — which is a sixth of that name and
        // has no business beating the word the user meant.
        let meant = score("uesr", &rec("user", "Query.user", Kind::Query)).unwrap();
        let accident = score(
            "uesr",
            &rec("closingIssuesReferences", "PullRequest.x", Kind::Field),
        )
        .unwrap();
        assert!(
            meant.score > accident.score,
            "user {} closingIssuesReferences {}",
            meant.score,
            accident.score
        );
    }

    #[test]
    fn kind_never_enters_the_score() {
        // A root field and the object it returns answer the query equally well.
        // As a term, kind weight's flat gap also bought name length in whatever
        // currency the tier was denominated in — and could outbid an exact name
        // once the bands stopped being 300 points apart.
        let root = score(
            "pokemon",
            &rec("pokemon", "query_root.pokemon", Kind::Query),
        )
        .unwrap();
        let object = score("pokemon", &rec("pokemon", "pokemon", Kind::Object)).unwrap();
        assert_eq!(root.score, object.score);
        let exact = score("car", &rec("CAR", "Icon.CAR", Kind::EnumValue)).unwrap();
        let prefix = score("car", &rec("card", "Mutation.card", Kind::Mutation)).unwrap();
        assert!(exact.score > prefix.score);
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
        assert!(
            user.score > account.score,
            "user {} > account {}",
            user.score,
            account.score
        );
        // the qualifier boost is the whole of the difference, and it stays out
        // of the merit the tail cut compares
        assert_eq!(user.merit, account.merit);
    }

    #[test]
    fn transposition_typo_still_matches_below_a_clean_hit() {
        // `usre` (transposed `user`) is not a subsequence of `User`, but a single
        // adjacent transposition should still match — ranked below a clean match.
        let clean = score("user", &rec("User", "Query.user", Kind::Object)).unwrap();
        let typo = score("usre", &rec("User", "Query.user", Kind::Object)).unwrap();
        assert!(
            clean.score > typo.score,
            "clean {} > typo {}",
            clean.score,
            typo.score
        );
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
        let phrase = ["cancel", "subscription"];
        let both = score_phrase(
            &phrase,
            &rec(
                "cancelSubscription",
                "Mutation.cancelSubscription",
                Kind::Mutation,
            ),
            score,
        );
        let one = score_phrase(
            &phrase,
            &rec("subscriptionPlan", "T.subscriptionPlan", Kind::Field),
            score,
        );
        assert_eq!(both.unwrap().matched, 2);
        assert_eq!(one.unwrap().matched, 1);
        assert!(score_phrase(&phrase, &rec("id", "T.id", Kind::Field), score).is_none());
    }

    #[test]
    fn a_word_only_the_description_carries_still_covers_it() {
        // What a schema calls `viewer` a person calls the current user, and
        // the description is where the schema says so.
        let phrase = ["current", "user"];
        let mut viewer = rec("viewer", "Query.viewer", Kind::Query);
        viewer.description = Some("The currently authenticated user.".into());
        let c = score_phrase(&phrase, &viewer, score).expect("described words count");
        assert_eq!(c.matched, 2, "both words are covered");
        assert_eq!(
            c.named, 0,
            "neither by the name — that's the bar it can't raise"
        );

        // A name match still outscores a described one by orders of magnitude,
        // so within one coverage tier the named record leads.
        let mut named = rec("currentUser", "Query.currentUser", Kind::Query);
        named.description = Some("Unrelated prose.".into());
        let n = score_phrase(&phrase, &named, score).expect("a name match");
        assert_eq!((n.named, n.matched), (2, 2));
        assert!(n.score.score > c.score.score * 100);
    }

    #[test]
    fn a_description_word_is_whole_not_a_fragment() {
        // `currently` says `current`; `recurrent` doesn't.
        assert!(described_by("current", "The currently authenticated user."));
        assert!(!described_by("current", "A recurrent billing cycle."));
        assert!(described_by("fork", "The number of forks."));
    }

    #[test]
    fn an_argument_is_matched_only_by_the_arg_scorer() {
        let mut field = rec("repository", "Query.repository", Kind::Query);
        field.args = vec!["followRenames: Boolean".into()];
        // the name pass doesn't see arguments at all — that's what lets the
        // caller run them as a fallback
        assert!(score("followRenames", &field).is_none());
        assert!(score_arg("followRenames", &field).is_some());
        // and the arg pass sees nothing else
        assert!(score_arg("repository", &field).is_none());
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
