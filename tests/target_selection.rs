//! Which record `-e` and `-R` will act on. Both turn the top hit into
//! something that reads as authoritative, so both act only on a field the
//! query actually named, and hand the pick back otherwise. Drives the real
//! binary: the guard, the exit status a script sees, and which stream each
//! part lands on are all invisible to a library test.
//!
//! The rejection path never reaches `rq`, so the `-R` cases here are hermetic.

mod common;

use std::process::{Command, Output};

const SCHEMA: &str = "examples/schema.graphql";

fn run(flag: &str, query: &str) -> Output {
    run_with(flag, query, &[])
}

fn run_with(flag: &str, query: &str, extra: &[&str]) -> Output {
    // Cargo will occasionally hand back a binary older than the source and
    // swear it's current; without this the failures look like real bugs.
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        // --fuzzy keeps this off the embedding model — the guard is about the
        // name typed, not about ranking, and a semantic build shouldn't make
        // the test slower or dependent on a downloaded model.
        .args([query, flag, "--fuzzy", SCHEMA])
        .args(extra)
        .output()
        .expect("gqls should be runnable")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout should be utf-8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr should be utf-8")
}

#[test]
fn drafts_for_the_field_the_query_names() {
    let out = run("-e", "Mutation.createUser");
    assert!(out.status.success(), "{}", stderr(&out));
    // The draft leads with the field's description, as a comment, so the whole
    // block is still one pasteable document.
    let text = stdout(&out);
    assert!(text.starts_with("# Create a user and return it."), "{text}");
    assert!(text.contains("mutation CreateUser("), "{text}");
}

#[test]
fn a_small_misspelling_still_counts_as_naming_it() {
    let out = run("-e", "createUesr");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("createUser(input: $input)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_corrected_spelling_says_which_field_it_settled_on() {
    // Answering a misspelt query as though it were typed correctly leaves the
    // user to notice the substitution from the output.
    let out = run("-e", "createUesr");
    assert_eq!(stderr(&out), "Did you mean Mutation.createUser?\n\n");
    // …and stdout stays a draft you could redirect straight into a file
    let text = stdout(&out);
    assert!(text.contains("mutation CreateUser("), "{text}");
    // an exactly-named field has nothing to correct
    let exact = run("-e", "Mutation.createUser");
    assert!(
        !stderr(&exact).contains("Did you mean"),
        "{}",
        stderr(&exact)
    );
}

#[test]
fn an_inexact_query_lists_the_candidates_instead_of_drafting() {
    // `crtusr` is a fine search query and a bad thing to draft from: an
    // operation looks authoritative enough to paste, so the pick is the
    // user's to make.
    let out = run("-e", "crtusr");
    assert!(!out.status.success(), "should not exit clean: {out:?}");
    let stdout = stdout(&out);
    assert!(!stdout.contains("mutation "), "drafted anyway: {stdout}");
    assert!(
        stdout.contains("Mutation.createUser"),
        "candidates missing: {stdout}"
    );
    assert_eq!(stderr(&out), "Did you mean:\n\n");
}

#[test]
fn resolve_holds_to_the_same_bar_as_drafting() {
    // A file:line answer is as authoritative as a drafted operation, and this
    // one is reached before rq is ever consulted.
    let out = run("-R", "crtusr");
    assert!(!out.status.success(), "should not exit clean: {out:?}");
    assert!(
        stdout(&out).contains("Mutation.createUser"),
        "candidates missing: {}",
        stdout(&out)
    );
    assert!(stderr(&out).contains("Did you mean:"), "{}", stderr(&out));
}

#[test]
fn json_output_stays_json() {
    // The prompts are for a reader, and reach them without a `-j` consumer
    // having to parse around prose.
    for flag in ["-e", "-R"] {
        let out = run_with(flag, "crtusr", &["-j"]);
        serde_json::from_str::<serde_json::Value>(&stdout(&out))
            .unwrap_or_else(|e| panic!("{flag} -j emitted non-JSON ({e}): {}", stdout(&out)));
    }
    // …and neither does a correction leak into it
    let out = run_with("-e", "createUesr", &["-j"]);
    let payload: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("a draft should still be JSON");
    assert_eq!(payload["path"], "Mutation.createUser");
}

#[test]
fn listing_a_types_fields_is_a_list_not_a_draft() {
    // `User.` enumerates — there's no single field to draft for.
    let out = run("-e", "User.");
    assert!(!out.status.success(), "should not exit clean: {out:?}");
    assert!(stdout(&out).contains("User.email"), "{}", stdout(&out));
}
