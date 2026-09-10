//! What a truncated list says about itself. `-l` silently drops matches, and
//! the rows that remain look exactly like a complete answer — so the count has
//! to reach the same person the list does. Drives the real binary, because the
//! notice lands on stderr while the list lands on stdout.

mod common;

use std::process::Command;

const SCHEMA: &str = "examples/schema.graphql";

/// `(stdout, stderr)` for a fuzzy run — off the embedding model, since what's
/// under test is the count, not the ranking.
fn run(args: &[&str]) -> (String, String) {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .args(["--fuzzy", SCHEMA])
        .output()
        .expect("gqls should be runnable");
    (
        String::from_utf8(out.stdout).expect("stdout should be utf-8"),
        String::from_utf8(out.stderr).expect("stderr should be utf-8"),
    )
}

#[test]
fn a_truncated_list_says_how_much_it_left_out() {
    let (stdout, stderr) = run(&["*", "-l", "5"]);
    assert_eq!(stdout.lines().count(), 5, "{stdout}");
    assert!(
        stderr.contains("matches; showing top 5"),
        "expected a count on stderr, got: {stderr:?}"
    );
}

#[test]
fn a_complete_list_says_nothing() {
    let (stdout, stderr) = run(&["Node.", "-l", "20"]);
    assert!(stdout.lines().count() < 20, "{stdout}");
    assert!(stderr.is_empty(), "{stderr:?}");
}

#[test]
fn explain_mode_reports_its_own_hidden_matches_and_not_the_limit() {
    // Naming one record collapses the list to it, which is not the limit
    // dropping anything — and `--no-explain`, not `-l`, is what brings the
    // rest back.
    let (_, stderr) = run(&["Post"]);
    assert!(stderr.contains("--no-explain"), "{stderr:?}");
    assert!(!stderr.contains("-l to adjust"), "{stderr:?}");
}

/// The path each row is about — column widths shift with the result set, so
/// only the leading token is comparable across two runs.
fn paths(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .collect()
}

#[test]
fn the_limit_does_not_decide_whether_the_answer_is_a_list() {
    // Reading "does this query name exactly one record" off the top `-l` rows
    // let the display limit turn a list into an authoritative single answer —
    // and, on a big schema, explain a different record at each limit.
    let (small, _) = run(&["user", "-l", "2"]);
    let (large, _) = run(&["user", "-l", "20"]);
    assert_eq!(paths(&small).len(), 2, "{small}");
    assert_eq!(paths(&small), paths(&large)[..2], "{small}\n---\n{large}");
}

#[test]
fn naming_one_record_explains_it_at_any_limit() {
    for limit in ["1", "20"] {
        let (stdout, stderr) = run(&["Role", "-l", limit]);
        assert!(stdout.starts_with("Role  [enum]"), "{stdout}");
        // and the count it sets aside is out of everything that matched
        assert!(stderr.contains("3 other matches"), "{stderr}");
    }
}

#[test]
fn an_empty_page_is_not_a_miss() {
    // `-l 0` shows nothing, which is not the same as nothing matching.
    let (stdout, stderr) = run(&["user", "-l", "0"]);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(!stderr.contains("no matches"), "{stderr}");
    assert!(stderr.contains("showing top 0"), "{stderr}");
}
