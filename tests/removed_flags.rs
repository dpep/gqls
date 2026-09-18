//! Flags gqls used to have fail as usage errors that say they were removed.
//! clap's own message for an unknown flag points elsewhere — at a similarly
//! spelled flag, or at `--` to search for the flag's text — and following it
//! exits 0 on nonsense.

mod common;

use std::process::Command;

#[test]
fn a_removed_flag_says_it_was_removed() {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    for args in [
        &["--semantic"][..],
        &["--fuzzy"],
        &["--warm"],
        &["--model", "all-MiniLM-L6-v2"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
            .args(["user", "examples/schema.graphql"])
            .args(args)
            .output()
            .expect("gqls should be runnable");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("was removed"), "{args:?}: {stderr}");
        assert!(stderr.contains("fuzzy"), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?} printed results");
    }
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
