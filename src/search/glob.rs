//! Wildcard (glob) matching for enumeration queries — `User.*`, `*.email`,
//! `get*`.
//!
//! Deliberately tiny: `*` is the only metacharacter, matching any run of
//! characters (`.` included, so `User.*` reaches nested paths). Everything
//! else is literal and the match is anchored at both ends, so a pattern says
//! exactly what it looks like. Case-insensitive, like the rest of search.

/// Whether `pattern` matches `text`. Linear-time two-pointer walk that
/// backtracks to the most recent `*` — no recursion, no regex dependency.
pub fn matches(pattern: &str, text: &str) -> bool {
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
        } else if pi < p.len() && p[pi] == t[ti] {
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
    // Trailing `*`s can match the empty remainder.
    p[pi..].iter().all(|&c| c == '*')
}

/// Whether a query is a wildcard pattern rather than a fuzzy search.
pub fn is_pattern(query: &str) -> bool {
    query.contains('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_matches_any_run_including_dots() {
        assert!(matches("User.*", "User.email"));
        assert!(matches("User.*", "User.posts.first")); // reaches nested paths
        assert!(matches("*.email", "User.email"));
        assert!(matches("*", "anything"));
        assert!(matches("*User*", "AdminUserProfile"));
    }

    #[test]
    fn matching_is_anchored_and_case_insensitive() {
        assert!(matches("user.*", "User.email"));
        assert!(matches("USER.*", "user.email"));
        // anchored: `User.*` must not match a type merely starting with User
        assert!(!matches("User.*", "UserProfile.email"));
        assert!(!matches("get*", "forget"));
        assert!(!matches("*email", "User.emails"));
    }

    #[test]
    fn a_starless_pattern_is_a_literal_equality() {
        assert!(matches("User.email", "user.EMAIL"));
        assert!(!matches("User.email", "User.email2"));
        assert!(!matches("User", "User.email"));
    }

    #[test]
    fn empty_and_star_only_edges() {
        assert!(matches("", ""));
        assert!(!matches("", "x"));
        assert!(matches("**", "x"));
        assert!(matches("a*", "a"));
    }

    #[test]
    fn is_pattern_detects_a_star() {
        assert!(is_pattern("User.*"));
        assert!(!is_pattern("User.email"));
    }
}
