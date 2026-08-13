//! Invariants of `--help`.
//!
//! It's the most-read text in the project and nothing else asserted anything
//! about it, so a fuzzy-only build spent an unknown stretch advertising
//! semantic search in `long_about` and disclaiming it forty lines later in
//! `EXAMPLES` — two hand-maintained copies of the same claim, drifting quietly.
//!
//! These don't pin wording. They pin the things that were actually wrong: that
//! the text matches the build it ships in, that every mode is discoverable, and
//! that a flag's description doesn't promise what the flag doesn't do.

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

/// Whether this binary has semantic search compiled in. The test binary and the
/// binary under test are built from the same feature set, so the cfg here
/// tracks the cfg there.
const SEMANTIC: bool = cfg!(feature = "_semantic");

#[test]
fn the_help_matches_the_build_it_ships_in() {
    let help = help();
    let claims_ranking = help.contains("ranked together");
    let disclaims = help.contains("not compiled into this build");
    assert_eq!(
        claims_ranking, SEMANTIC,
        "a build should advertise semantic ranking only if it has it:\n{help}"
    );
    assert_eq!(
        disclaims, !SEMANTIC,
        "and disclaim it only if it hasn't:\n{help}"
    );
    assert!(
        !(claims_ranking && disclaims),
        "help contradicted itself:\n{help}"
    );
}

/// Whether `flag` is *listed as an option*, rather than merely mentioned.
///
/// Token equality, because neither a substring search nor a prefix match works:
/// the lean build's examples name `--semantic` in the line explaining it isn't
/// compiled in, and clap renders a flag with a short form as `-e, --example`,
/// so the long name isn't at the start of its own line.
fn lists_flag(help: &str, flag: &str) -> bool {
    help.lines()
        .any(|line| line.split([' ', ',', '=']).any(|token| token == flag))
}

#[test]
fn semantic_only_flags_are_hidden_exactly_when_they_are_useless() {
    let help = help();
    // These do nothing without the feature.
    for flag in ["--semantic", "--model", "--warm", "--fuzzy"] {
        assert_eq!(
            lists_flag(&help, flag),
            SEMANTIC,
            "{flag} should be listed only on a semantic build:\n{help}"
        );
    }
    // These were hidden with them, and shouldn't have been: both work, and do
    // more than embedding, on every build.
    for flag in ["--clear-cache", "--refresh"] {
        assert!(
            lists_flag(&help, flag),
            "{flag} works on every build:\n{help}"
        );
    }
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
    for flag in ["--example", "--resolve", "--json", "--no-explain"] {
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
