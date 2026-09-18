//! Flags gqls used to have fail as usage errors that say they were removed.
//! clap's own message for an unknown flag points elsewhere — at a similarly
//! spelled flag, or at `--` to search for the flag's text — and following it
//! exits 0 on nonsense.

mod common;

use std::process::Command;

#[test]
fn a_removed_flag_gqls_cannot_honour_fails_and_says_why() {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    for (args, why) in [
        (&["--semantic"][..], "ranks by name only"),
        (&["--warm"], "nothing to pre-build"),
        (&["--model", "all-MiniLM-L6-v2"], "no model to choose"),
        (&["--model=all-MiniLM-L6-v2"], "no model to choose"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
            .args(["user", "examples/schema.graphql"])
            .args(args)
            .output()
            .expect("gqls should be runnable");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("was removed"), "{args:?}: {stderr}");
        assert!(stderr.contains(why), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?} printed results");
    }
}

#[test]
fn fuzzy_still_runs_and_says_it_does_nothing() {
    // It asked for what every query now gets, so an old script keeps working.
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_gqls"))
            .args(args)
            .output()
            .expect("gqls should be runnable")
    };
    let plain = run(&["user", "examples/schema.graphql", "-J"]);
    let fuzzy = run(&["user", "--fuzzy", "examples/schema.graphql", "-J"]);
    let stderr = String::from_utf8_lossy(&fuzzy.stderr);
    assert!(fuzzy.status.success(), "{stderr}");
    assert_eq!(plain.stdout, fuzzy.stdout);
    assert!(stderr.contains("--fuzzy does nothing"), "{stderr}");

    // -q silences it like any other status line.
    let quiet = run(&["user", "--fuzzy", "-q", "examples/schema.graphql"]);
    assert!(
        quiet.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&quiet.stderr)
    );
}

#[test]
fn removed_flags_stay_out_of_help_and_completions() {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    for args in [&["--help"][..], &["--completions", "zsh"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
            .args(args)
            .output()
            .expect("gqls should be runnable");
        let text = String::from_utf8_lossy(&out.stdout);
        for flag in ["--semantic", "--fuzzy", "--warm", "--model"] {
            assert!(!text.contains(flag), "{flag} is listed by {args:?}");
        }
    }
}

#[test]
fn a_query_after_the_separator_is_still_a_query() {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(["examples/schema.graphql", "--", "--fuzzy"])
        .output()
        .expect("gqls should be runnable");
    assert_eq!(out.status.code(), Some(0));
}
