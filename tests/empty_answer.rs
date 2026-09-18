//! What a miss says about itself. An empty answer is the one result that
//! carries no evidence of how it was reached, so everything that shaped it —
//! the flags, and a schema nobody chose — has to be in the sentence. Drives
//! the real binary: the message lands on stderr, and the discovered-schema
//! case depends on the process's working directory.

mod common;

use std::process::Command;

const SCHEMA: &str = "examples/schema.graphql";

/// `(stdout, stderr)` for a run against the bundled schema.
fn run_both(args: &[&str]) -> (String, String) {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .arg(SCHEMA)
        .output()
        .expect("gqls should be runnable");
    (
        String::from_utf8(out.stdout).expect("stdout should be utf-8"),
        String::from_utf8(out.stderr).expect("stderr should be utf-8"),
    )
}

fn run(args: &[&str]) -> String {
    run_both(args).1
}

/// stderr for a run against a schema other than the bundled one.
fn run_against(schema: &str, args: &[&str]) -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .args(["--refresh", schema])
        .output()
        .expect("gqls should be runnable");
    String::from_utf8(out.stderr).expect("stderr should be utf-8")
}

#[test]
fn a_miss_names_the_filter_that_emptied_it() {
    // "nothing returns Post" is a claim about the schema, and it's false —
    // several fields return one, and `-k mutation` is what ruled them out.
    let (stdout, _) = run_both(&["--returns", "Post"]);
    let without = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(without > 0, "the unfiltered filter should match: {stdout}");

    let stderr = run(&["--returns", "Post", "-k", "mutation"]);
    assert!(stderr.contains("-k mutation"), "{stderr}");
    let noun = if without == 1 { "match" } else { "matches" };
    assert!(
        stderr.contains(&format!("{without} {noun} without it")),
        "should report what dropping the filter finds: {stderr}"
    );
}

#[test]
fn a_miss_with_nothing_to_relax_stays_a_plain_no() {
    let stderr = run(&["zzzz"]);
    assert_eq!(stderr, "gqls: no matches for \"zzzz\"\n");
}

#[test]
fn a_lone_ndjson_miss_is_a_row_rather_than_silence() {
    // Zero rows on a row-per-line stream is zero bytes, so a single `-J` query
    // that matched nothing was indistinguishable from one that never ran. A
    // batch already got the sentinel; the lone query is the same problem.
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(["zzzz", SCHEMA, "-J"])
        .output()
        .expect("gqls should be runnable");
    let stdout = String::from_utf8(out.stdout).expect("stdout should be utf-8");
    let miss: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    assert_eq!(miss["status"], "no_matches");
    // No `query` key: the rows carry one only in a batch, where there is
    // something to tell apart.
    assert!(miss.get("query").is_none(), "{miss}");

    // `-j` says it with an empty array, which is already a whole answer — so
    // that shape stays exactly as it was.
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(["zzzz", SCHEMA, "-j"])
        .output()
        .expect("gqls should be runnable");
    assert_eq!(
        String::from_utf8(out.stdout)
            .expect("stdout should be utf-8")
            .trim(),
        "[]"
    );
}

#[test]
fn a_miss_names_a_schema_nobody_chose() {
    // Discovery is the common path, and the schema it settles on is an
    // assumption behind every answer. A miss is where it decides between
    // "not in this schema" and "you're searching the wrong one".
    let dir = std::env::temp_dir().join("gqls-empty-answer-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::copy(SCHEMA, dir.join("schema.graphql")).expect("a schema to discover");

    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        // --refresh so a remembered answer from an earlier run can't stand in
        // for the walk this is about.
        .args(["zzzz", "--refresh"])
        .current_dir(&dir)
        .output()
        .expect("gqls should be runnable");
    let stderr = String::from_utf8(out.stderr).expect("stderr should be utf-8");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        stderr.contains("no matches for \"zzzz\" in schema.graphql"),
        "{stderr}"
    );
    // …and a schema the caller named needs no introduction
    assert!(!run(&["zzzz"]).contains(" in "), "{stderr}");
}

#[test]
fn a_draft_too_large_to_paste_says_so() {
    // A draft nobody can paste didn't answer the question, and its size is
    // invisible until it has already scrolled past. Keyed off the rendered
    // size rather than off `--depth`, because depth alone says nothing: a
    // small schema drafts usefully at the cap, and a wide one blows past it
    // in three levels.
    let stderr = run_against(
        "tests/fixtures/wide_fanout.graphql",
        &["Query.root", "-e", "--depth", "4"],
    );
    assert!(
        stderr.contains("this draft is") && stderr.contains("--depth"),
        "expected a size warning naming the lever, got: {stderr:?}"
    );

    // The same schema one level shallower is an ordinary draft; silence there
    // is what stops the warning becoming noise everyone learns to ignore.
    let quiet = run_against(
        "tests/fixtures/wide_fanout.graphql",
        &["Query.root", "-e", "--depth", "2"],
    );
    assert!(!quiet.contains("this draft is"), "{quiet:?}");
}
