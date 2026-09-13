//! What a miss says about itself. An empty answer is the one result that
//! carries no evidence of how it was reached, so everything that shaped it —
//! the flags, and a schema nobody chose — has to be in the sentence. Drives
//! the real binary: the message lands on stderr, and the discovered-schema
//! case depends on the process's working directory.

mod common;

use std::process::Command;

const SCHEMA: &str = "examples/schema.graphql";

/// `(stdout, stderr)` for a fuzzy run against the bundled schema.
fn run_both(args: &[&str]) -> (String, String) {
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

fn run(args: &[&str]) -> String {
    run_both(args).1
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
    assert!(
        stderr.contains(&format!("{without} match without it")),
        "should report what dropping the filter finds: {stderr}"
    );
}

#[test]
fn a_miss_with_nothing_to_relax_stays_a_plain_no() {
    let stderr = run(&["zzzz"]);
    assert_eq!(stderr, "gqls: no matches for \"zzzz\"\n");
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
        .args(["zzzz", "--fuzzy", "--refresh"])
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
