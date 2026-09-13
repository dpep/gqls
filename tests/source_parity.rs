//! The same schema, loaded two ways, answers the same questions the same way.
//!
//! `fixtures/parity.json` is the introspection dump of `fixtures/parity.graphql`,
//! produced by graphql-ruby — a real server's answer, not one gqls wrote:
//!
//! ```ruby
//! require "graphql"; require "json"
//! schema = GraphQL::Schema.from_definition(File.read("tests/fixtures/parity.graphql"))
//! # to_json returns a *string*; writing it straight out double-encodes the file
//! File.write("tests/fixtures/parity.json", JSON.pretty_generate(JSON.parse(schema.to_json)))
//! ```
//!
//! Every field of every record must match across the pair. The one exception is
//! `directives`, and it is asserted rather than skipped: standard introspection
//! exposes directive *definitions* but not their applications, so that column is
//! empty from a dump however many `@auth`s the SDL applies.

mod common;

use std::collections::BTreeMap;

use gqls::load::{load, LoadOptions};
use gqls::model::SchemaRecord;

fn records(source: &str) -> BTreeMap<String, SchemaRecord> {
    // --refresh: these fixtures are stable, so a stale record cache from an
    // earlier build of the loaders would otherwise answer for them.
    let opts = LoadOptions {
        refresh: true,
        ..Default::default()
    };
    load(source, &opts)
        .unwrap_or_else(|e| panic!("{source} should load: {e}"))
        .into_iter()
        .map(|r| (r.path.clone(), r))
        .collect()
}

#[test]
fn a_schema_answers_the_same_from_its_sdl_and_its_introspection_dump() {
    let sdl = records("tests/fixtures/parity.graphql");
    let dump = records("tests/fixtures/parity.json");
    assert!(sdl.len() > 20, "the fixture should be worth diffing");

    for (path, a) in &sdl {
        let b = dump
            .get(path)
            .unwrap_or_else(|| panic!("{path} is in the SDL but not the dump"));
        assert_eq!(a.kind, b.kind, "{path}: kind");
        assert_eq!(a.parent, b.parent, "{path}: parent");
        assert_eq!(a.type_ref, b.type_ref, "{path}: type_ref");
        assert_eq!(a.args, b.args, "{path}: args");
        assert_eq!(a.arg_descriptions, b.arg_descriptions, "{path}: arg docs");
        assert_eq!(a.description, b.description, "{path}: description");
        assert_eq!(a.deprecated, b.deprecated, "{path}: deprecated");
        assert_eq!(a.default, b.default, "{path}: default");
        // Order is the schema's in SDL and the server's in a dump; membership
        // is the fact, so compare it as a set.
        assert_eq!(
            sorted(&a.possible_types),
            sorted(&b.possible_types),
            "{path}: possible_types"
        );
    }

    // The dump also carries what a schema doesn't write down: the built-in
    // scalars and directives. They're additions, never disagreements.
    for path in dump.keys() {
        assert!(
            sdl.contains_key(path)
                || matches!(path.as_str(), "String" | "Int" | "Float" | "Boolean" | "ID")
                || path.starts_with('@'),
            "{path} came from the dump alone"
        );
    }
}

#[test]
fn applied_directives_are_the_one_thing_a_dump_cannot_say() {
    let sdl = records("tests/fixtures/parity.graphql");
    let dump = records("tests/fixtures/parity.json");

    // The SDL applies @auth to a type and a field…
    assert_eq!(sdl["User"].directives, ["@auth(requires: MEMBER)"]);
    assert_eq!(sdl["User.secret"].directives, ["@auth(requires: ADMIN)"]);

    // …and introspection reports none of it, anywhere. The definition survives
    // — it's the *application* that has nowhere to live in the protocol.
    assert!(
        dump.values().all(|r| r.directives.is_empty()),
        "introspection has no way to report an applied directive"
    );
    assert!(dump.contains_key("@auth"), "its definition still arrives");

    // @deprecated is the exception the protocol does carry, by a side channel
    // (isDeprecated/deprecationReason) — so this one fact does survive.
    assert_eq!(
        dump["User.email"].deprecated.as_deref(),
        Some("Use contact.")
    );
}

/// …and gqls says so, rather than letting an empty column speak for itself.
#[test]
fn the_dumps_blind_spot_is_disclosed_under_v() {
    let caveat = "introspection reports no applied directives";

    let dump = run(&["secret", "tests/fixtures/parity.json", "--fuzzy", "-v"]);
    assert!(dump.contains(caveat), "{dump}");
    assert!(dump.contains("@auth"), "it names the evidence: {dump}");

    // The SDL of the same schema has nothing to disclose…
    let sdl = run(&["secret", "tests/fixtures/parity.graphql", "--fuzzy", "-v"]);
    assert!(!sdl.contains(caveat), "{sdl}");

    // …and neither has a normal run: it's a diagnostic, and a federated
    // endpoint would otherwise carry this line all day.
    let quiet = run(&["secret", "tests/fixtures/parity.json", "--fuzzy"]);
    assert!(!quiet.contains(caveat), "{quiet}");
}

/// The binary's stderr for one invocation.
fn run(args: &[&str]) -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .arg("--refresh")
        .output()
        .expect("gqls should be runnable");
    String::from_utf8(out.stderr).expect("stderr should be utf-8")
}

fn sorted(v: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = v.iter().map(String::as_str).collect();
    v.sort_unstable();
    v
}
