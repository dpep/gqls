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
