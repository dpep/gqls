//! A "no" says what happened and points somewhere useful. Each case here was a
//! refusal or a fallback that used to be silent or to misdirect.

mod common;

use std::process::{Command, Output};

const SCHEMA: &str = "examples/schema.graphql";

fn gqls(args: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .output()
        .expect("gqls should be runnable")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_path_that_isnt_a_schema_source_is_refused_not_searched_for() {
    // It used to join the query, and discovery answered from another schema.
    let out = gqls(&["user", "examples/schema.grapqhl", "-j"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("examples/schema.grapqhl"), "{err}");
    assert!(
        err.contains(".graphql"),
        "should say what a source looks like: {err}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn an_unresolved_qualifier_says_the_search_went_unscoped() {
    // A qualifier naming no type falls back to matching the whole path, which
    // is documented — and was silent, so the scope looked applied.
    let out = gqls(&["Zzzz.name", SCHEMA]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("no type named \"Zzzz\""), "{err}");
    assert!(err.contains("every type"), "{err}");
}

#[test]
fn returns_given_an_enum_value_or_directive_says_what_it_is() {
    for (value, says) in [
        ("ADMIN", "a value of enum Role"),
        ("Role.ADMIN", "a value of enum Role"),
        ("auth", "a directive"),
    ] {
        let out = gqls(&["--returns", value, SCHEMA]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{value}: {err}");
        assert!(err.contains(says), "{value}: {err}");
        assert!(err.contains("--returns takes a type"), "{value}: {err}");
    }
}

#[test]
fn returns_given_a_field_of_any_kind_points_at_drafting_it() {
    for field in ["User.name", "CreateUserInput.email", "Mutation.createUser"] {
        let out = gqls(&["--returns", field, SCHEMA]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "{field}: {err}");
        assert!(err.contains(&format!("gqls {field} -e")), "{field}: {err}");
        assert!(
            !err.contains("fetches it"),
            "not every field is fetched: {err}"
        );
    }
}

#[test]
fn fuzzy_with_a_value_is_dropped_like_bare_fuzzy() {
    let out = gqls(&["user", SCHEMA, "--fuzzy=x"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("--fuzzy does nothing"), "{err}");
}
