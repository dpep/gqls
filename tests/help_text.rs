//! Invariants of `--help`.
//!
//! It's the most-read text in the project, and drift in it is silent.
//!
//! These don't pin wording. They pin the things that were actually wrong: that
//! every mode is discoverable, and that a flag's description doesn't promise
//! what the flag doesn't do.

mod common;

use std::process::Command;

fn help() -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg("--help")
        .output()
        .expect("gqls --help should run");
    assert!(out.status.success(), "--help should exit 0");
    String::from_utf8(out.stdout).expect("help should be utf-8")
}

#[test]
fn via_is_refused_without_the_flag_it_routes() {
    // Its help says it routes --example, and it does nothing else. A flag that
    // silently no-ops is the promise this file exists to keep it from making.
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(["Query.user", "--via", "Query.user"])
        .arg("examples/schema.graphql")
        .output()
        .expect("gqls should be runnable");

    assert!(!out.status.success(), "--via alone should be refused");
    let err = String::from_utf8(out.stderr).expect("stderr should be utf-8");
    assert!(err.contains("--example"), "{err}");
}

/// Whether `flag` is *listed as an option*, rather than merely mentioned.
///
/// Token equality, because neither a substring search nor a prefix match works:
/// a flag can be mentioned in another's description, and clap renders a flag
/// with a short form as `-e, --example`, so the long name isn't at the start of
/// its own line.
fn lists_flag(help: &str, flag: &str) -> bool {
    help.lines()
        .any(|line| line.split([' ', ',', '=']).any(|token| token == flag))
}

#[test]
fn every_mode_is_discoverable_from_help_alone() {
    let help = help();
    // Explain mode was reachable only via `--no-explain`'s description — the
    // base behaviour documented as the thing you turn off.
    assert!(
        help.contains("explains it"),
        "help should describe what a named record does:\n{help}"
    );
    for flag in [
        "--example",
        "--resolve",
        "--json",
        "--no-explain",
        "--clear-cache",
        "--refresh",
    ] {
        assert!(lists_flag(&help, flag), "{flag} missing from help:\n{help}");
    }
}

#[test]
fn the_examples_teach_the_current_argument_syntax() {
    let help = help();
    assert!(
        help.contains("gqls query user"),
        "a leading kind word should be shown:\n{help}"
    );
    // Multi-word queries stopped needing quotes; an example that quotes one
    // teaches the opposite of what the QUERY description now says.
    assert!(
        help.contains("gqls cancel a subscription"),
        "the multi-word example should be unquoted:\n{help}"
    );
}

#[test]
fn no_flag_claims_to_act_on_the_top_match() {
    // `-e`/`-R` refuse anything but a named record. "Top match" described the
    // behaviour they deliberately don't have.
    let help = help();
    assert!(
        !help.contains("top match"),
        "-e/-R act on a named record, not the top match:\n{help}"
    );
}
