//! A spelling that's already a word in the schema isn't a misspelling. `star`
//! sits whole in `addStar`, so correcting it to `start` and explaining or
//! drafting that answered a question nobody asked — confidently, with the real
//! answer hidden behind "N other matches".

mod common;

use std::process::{Command, Output};

const SCHEMA: &str = "tests/fixtures/word_not_typo.graphql";

fn gqls(args: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .arg(SCHEMA)
        .output()
        .expect("gqls should be runnable")
}

#[test]
fn a_schema_word_lists_rather_than_explaining_a_lookalike() {
    let out = gqls(&["star", "-J"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let paths: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.split("\"path\":\"").nth(1)?.split('"').next())
        .collect();
    assert!(paths.contains(&"Mutation.addStar"), "{stdout}");
    assert!(
        !stdout.contains("\"match\""),
        "explained a lookalike: {stdout}"
    );
}

#[test]
fn a_schema_word_is_not_drafted_as_a_lookalike() {
    let out = gqls(&["star", "-e"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Did you mean:"), "{stderr}");
    assert!(!stderr.contains("Did you mean Query.start?"), "{stderr}");
}

#[test]
fn a_real_misspelling_is_still_corrected() {
    let out = gqls(&["strat", "-e"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("Did you mean Query.start?"), "{stderr}");
}

#[test]
fn a_misspelled_qualifier_is_corrected_even_when_the_field_is_a_word() {
    // `start` is a schema word; `Qeury` is what was misspelled.
    let out = gqls(&["Qeury.start", "-e"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("Did you mean Query.start?"), "{stderr}");
}
