//! End-to-end checks on `--example`: load the bundled schema the way the CLI
//! does, draft operations, and hold them to the contract that matters — the
//! output has to be GraphQL a server will accept, not merely a plausible
//! string. Hermetic; the live-endpoint counterpart lives in
//! `http_introspection.rs`.

use gqls::example;
use gqls::load::{self, LoadOptions};
use gqls::model::SchemaRecord;

const SCHEMA: &str = "examples/schema.graphql";

fn records() -> Vec<SchemaRecord> {
    // `refresh` skips the parsed-record cache so the test reads the file, not
    // whatever a previous run left behind.
    let opts = LoadOptions {
        refresh: true,
        ..Default::default()
    };
    load::load(SCHEMA, &opts).expect("the bundled example schema should load")
}

fn draft(path: &str) -> example::Example {
    let records = records();
    let target = records
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{path} missing from {SCHEMA}"));
    example::build(target, &records).expect("drafting should succeed")
}

#[test]
fn drafts_a_root_query_with_typed_variables() {
    let ex = draft("Query.user");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(
        ex.operation.starts_with("query User($id: ID!) {"),
        "{}",
        ex.operation
    );
    // the placeholder names its type, so it can't pass for a real value
    assert_eq!(ex.variables, serde_json::json!({ "id": "<ID!>" }));
    // one level of leaves, with the object-valued field left as a marker
    assert!(ex.operation.contains("\n    email\n"), "{}", ex.operation);
    assert!(
        ex.operation.contains("# posts: Post — add fields you need"),
        "{}",
        ex.operation
    );
}

#[test]
fn expands_input_objects_and_the_enums_they_reference() {
    let ex = draft("Mutation.createUser");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    let blocks: Vec<String> = ex.input_types.iter().map(|b| b.join("\n")).collect();
    let input = blocks
        .iter()
        .find(|b| b.starts_with("CreateUserInput {"))
        .unwrap_or_else(|| panic!("no CreateUserInput block in {blocks:?}"));
    assert!(input.contains("name: String!"), "{input}");
    assert!(input.contains("role: Role"), "{input}");

    // Role is reached only through CreateUserInput.role — the expansion has to
    // follow input fields, not just the argument list
    assert!(
        blocks.iter().any(|b| b.starts_with("Role = ")),
        "enum not expanded: {blocks:?}"
    );
}

#[test]
fn omits_a_deprecated_fields_arguments_it_does_not_need() {
    // Mutation.deleteUser(id: ID!) — required, so it must appear
    let ex = draft("Mutation.deleteUser");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert!(
        ex.operation.starts_with("mutation DeleteUser($id: ID!) {"),
        "{}",
        ex.operation
    );
    // Boolean! return: a leaf, so no selection set
    assert!(
        ex.operation.contains("deleteUser(id: $id)\n"),
        "{}",
        ex.operation
    );
}

#[test]
fn wraps_a_nested_field_in_a_root_that_returns_its_type() {
    // User.email isn't callable on its own. Both Query.user(id: ID!) and
    // Query.users return a User, and the one needing no arguments wins — the
    // drafted query then runs as-is, with `Query.user` offered as the
    // alternative.
    let ex = draft("User.email");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(ex.via.as_deref(), Some("Query.users"));
    assert!(
        ex.alternatives.iter().any(|a| a == "Query.user"),
        "{:?}",
        ex.alternatives
    );
    assert!(ex.operation.contains("users {"), "{}", ex.operation);
    assert!(ex.operation.contains("email"), "{}", ex.operation);
    assert_eq!(ex.variables, serde_json::json!({}));
}

#[test]
fn a_union_return_is_still_a_runnable_query() {
    // Query.search returns SearchResult, a union — selecting its fields
    // directly wouldn't parse, so it has to fall back to __typename
    let ex = draft("Query.search");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert!(ex.operation.contains("__typename"), "{}", ex.operation);
}
