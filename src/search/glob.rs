//! Wildcard (glob) matching for enumeration queries — `User.*`, `*.email`,
//! `get*`, `User.?d`, `User.{first,last}Name`.
//!
//! Deliberately small: `*` matches any run of characters, `?` exactly one
//! (both spanning `.`, so `User.*` reaches nested paths), and `{a,b}` expands
//! to alternatives shell-style. Everything else is literal and matching is
//! anchored at both ends, so a pattern says exactly what it looks like.
//! Case-insensitive, like the rest of search. There's no escape syntax —
//! GraphQL names can't contain these characters anyway.

/// Cap on brace expansion, so a pathological pattern can't blow up. Real
/// patterns produce a handful of alternatives; hitting this is reported, never
/// silently truncated.
const MAX_ALTERNATIVES: usize = 64;

/// A parsed wildcard pattern: brace groups expanded to their alternatives, any
/// of which may match. Built once per query, then tested against every record.
pub struct Pattern {
    alternatives: Vec<String>,
}

impl Pattern {
    /// Expand `pattern`'s brace groups. Malformed braces (unbalanced, or a `}`
    /// before its `{`) are left literal rather than rejected — a query is a
    /// user's guess, not a program.
    pub fn new(pattern: &str) -> Self {
        // `User.` is shorthand for `User.*` — a trailing dot reads as "and
        // what's inside", and needs no shell quoting the way a bare `*` does.
        let shorthand = trailing_dot(pattern).then(|| format!("{pattern}*"));
        let pattern = shorthand.as_deref().unwrap_or(pattern);
        let mut alternatives = Vec::new();
        expand(pattern, &mut alternatives);
        // `expand` stops one past the cap, so this distinguishes "exactly at
        // the cap" (fine) from "more were wanted" (reported, never silent).
        if alternatives.len() > MAX_ALTERNATIVES {
            alternatives.truncate(MAX_ALTERNATIVES);
            crate::status!(
                "pattern expands past {MAX_ALTERNATIVES} alternatives — \
                 matching the first {MAX_ALTERNATIVES}"
            );
        }
        Self { alternatives }
    }

    /// Whether any alternative matches `text`.
    pub fn matches(&self, text: &str) -> bool {
        self.alternatives.iter().any(|p| matches_one(p, text))
    }

    /// Whether this pattern addresses qualified paths (`User.*`) rather than
    /// leaf names (`get*`) — true when any alternative contains a `.`.
    pub fn targets_path(&self) -> bool {
        self.alternatives.iter().any(|a| a.contains('.'))
    }
}

/// Whether a query is a wildcard pattern rather than something to search for.
///
/// Patterns are single tokens: a query containing whitespace is prose, so a
/// natural-language phrase ending in `?` ("how do I cancel a subscription?")
/// stays a semantic query instead of becoming a glob that matches nothing.
pub fn is_pattern(query: &str) -> bool {
    if query.split_whitespace().count() != 1 {
        return false;
    }
    query.contains('*')
        || query.contains('?')
        || trailing_dot(query)
        || query
            .find('{')
            .zip(query.rfind('}'))
            .is_some_and(|(open, close)| open < close)
}

/// Whether a query uses the `User.` shorthand for `User.*`. A lone `.` doesn't
/// count — there's no type named there, so it stays an ordinary (fruitless)
/// search rather than a pattern matching nothing.
fn trailing_dot(query: &str) -> bool {
    query.len() > 1 && query.ends_with('.')
}

/// Match a single brace-free pattern: linear two-pointer walk that backtracks
/// to the most recent `*` — no recursion, no regex dependency.
fn matches_one(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // The last `*` seen, and where in `text` we resumed after it — together
    // these let a failed match retry with the `*` swallowing one more char.
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            resume = ti;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            // Mismatch after a `*`: let it absorb one more char and retry.
            pi = s + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    // Trailing `*`s can match the empty remainder; a `?` still needs a char.
    p[pi..].iter().all(|&c| c == '*')
}

/// Recursively expand the first brace group, appending finished patterns.
fn expand(pattern: &str, out: &mut Vec<String>) {
    if out.len() > MAX_ALTERNATIVES {
        return;
    }
    let Some((open, close)) = first_group(pattern) else {
        out.push(pattern.to_string());
        return;
    };
    let (prefix, suffix) = (&pattern[..open], &pattern[close + 1..]);
    for alt in split_alternatives(&pattern[open + 1..close]) {
        expand(&format!("{prefix}{alt}{suffix}"), out);
        // One past the cap, matching the top guard — `Pattern::new` needs that
        // extra element to tell "at the cap" from "truncated" and report it.
        if out.len() > MAX_ALTERNATIVES {
            return;
        }
    }
}

/// Byte offsets of the first `{` and its matching `}`, honoring nesting.
fn first_group(pattern: &str) -> Option<(usize, usize)> {
    let open = pattern.find('{')?;
    let mut depth = 0usize;
    for (i, c) in pattern[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + i));
                }
            }
            _ => {}
        }
    }
    None // unbalanced — treat the `{` as a literal
}

/// Split a brace group's body on top-level commas, ignoring nested groups.
fn split_alternatives(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, text: &str) -> bool {
        Pattern::new(pattern).matches(text)
    }

    #[test]
    fn star_matches_any_run_including_dots() {
        assert!(m("User.*", "User.email"));
        assert!(m("User.*", "User.posts.first")); // reaches nested paths
        assert!(m("*.email", "User.email"));
        assert!(m("*", "anything"));
        assert!(m("*User*", "AdminUserProfile"));
    }

    #[test]
    fn question_mark_matches_exactly_one_char() {
        assert!(m("User.?d", "User.id"));
        assert!(m("???", "abc"));
        assert!(!m("???", "ab"));
        assert!(!m("???", "abcd"));
        assert!(!m("User.?", "User.id")); // one char, not two
        assert!(m("User.??", "User.id"));
    }

    #[test]
    fn braces_expand_to_alternatives() {
        assert!(m("User.{first,last}Name", "User.firstName"));
        assert!(m("User.{first,last}Name", "User.lastName"));
        assert!(!m("User.{first,last}Name", "User.middleName"));
        // combines with the other metacharacters
        assert!(m("{User,Company}.*", "Company.employees"));
        assert!(m("*.{id,uuid}", "Order.uuid"));
    }

    #[test]
    fn braces_nest_and_survive_malformed_input() {
        assert!(m("User.{a,{b,c}}", "User.c"));
        // unbalanced braces are literal, not an error
        assert!(m("User.{a", "User.{a"));
        assert!(!m("User.{a", "User.a"));
    }

    #[test]
    fn matching_is_anchored_and_case_insensitive() {
        assert!(m("user.*", "User.email"));
        assert!(m("USER.*", "user.email"));
        // anchored: `User.*` must not match a type merely starting with User
        assert!(!m("User.*", "UserProfile.email"));
        assert!(!m("get*", "forget"));
        assert!(!m("*email", "User.emails"));
    }

    #[test]
    fn a_metacharacter_free_pattern_is_a_literal_equality() {
        assert!(m("User.email", "user.EMAIL"));
        assert!(!m("User.email", "User.email2"));
        assert!(!m("User", "User.email"));
    }

    #[test]
    fn empty_and_star_only_edges() {
        assert!(m("", ""));
        assert!(!m("", "x"));
        assert!(m("**", "x"));
        assert!(m("a*", "a"));
    }

    #[test]
    fn trailing_dot_is_shorthand_for_dot_star() {
        assert!(is_pattern("User."));
        assert!(m("User.", "User.email"));
        assert!(m("User.", "User.posts.first"));
        // same anchoring as the long form
        assert!(!m("User.", "UserProfile.email"));
        assert!(!m("User.", "User"));
        // a lone dot isn't a pattern
        assert!(!is_pattern("."));
    }

    #[test]
    fn targets_path_follows_the_dot() {
        assert!(Pattern::new("User.*").targets_path());
        assert!(!Pattern::new("get*").targets_path());
        assert!(Pattern::new("{User.id,name}").targets_path());
    }

    #[test]
    fn is_pattern_detects_metacharacters_but_not_prose() {
        assert!(is_pattern("User.*"));
        assert!(is_pattern("User.?d"));
        assert!(is_pattern("User.{a,b}"));
        assert!(!is_pattern("User.email"));
        // a phrase stays a search, even with a trailing question mark
        assert!(!is_pattern("how do I cancel a subscription?"));
        assert!(!is_pattern("what does * mean"));
    }

    #[test]
    fn expansion_is_capped() {
        // 2^8 = 256 combinations requested; the cap holds
        let pattern = "{a,b}".repeat(8);
        assert_eq!(Pattern::new(&pattern).alternatives.len(), MAX_ALTERNATIVES);
        // a pattern landing exactly on the cap keeps every alternative
        let exact = "{a,b}".repeat(6); // 2^6 = 64
        assert_eq!(Pattern::new(&exact).alternatives.len(), MAX_ALTERNATIVES);
    }
}
