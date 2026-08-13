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
    draft_deep(path, 1)
}

fn draft_deep(path: &str, depth: usize) -> example::Example {
    let records = records();
    let target = records
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{path} missing from {SCHEMA}"));
    example::build(target, &records, depth).expect("drafting should succeed")
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
        ex.operation.contains("# posts: Post { … }"),
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
fn a_union_return_becomes_inline_fragments_over_its_members() {
    // Query.search returns SearchResult = User | Post. A union has no fields
    // of its own, so the only form a server accepts is inline fragments.
    let ex = draft("Query.search");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(ex.operation.contains("__typename"), "{}", ex.operation);
    assert!(ex.operation.contains("... on User {"), "{}", ex.operation);
    assert!(ex.operation.contains("... on Post {"), "{}", ex.operation);
    // each member selected to the same depth as any object return
    assert!(ex.operation.contains("email"), "{}", ex.operation);
    assert!(ex.operation.contains("title"), "{}", ex.operation);
}

#[test]
fn an_interface_reaches_its_implementors_extra_fields() {
    // The bundled schema has no interface, so this one is built inline: the
    // interface's own fields are common to all implementors, and the fields
    // worth querying usually live on the concrete types.
    let sdl = "\
        type Query { node(id: ID!): Node }\n\
        interface Node { id: ID! createdAt: String! }\n\
        type Article implements Node { id: ID! createdAt: String! headline: String! }\n\
        type Video implements Node { id: ID! createdAt: String! streamUrl: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.node").unwrap();
    let ex = example::build(target, &records, 1).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    // common fields selected once, on the interface itself
    assert!(ex.operation.contains("\n    id\n"), "{}", ex.operation);
    assert!(ex.operation.contains("createdAt"), "{}", ex.operation);
    // and each implementor contributes only what it adds
    assert!(
        ex.operation.contains("... on Article {"),
        "{}",
        ex.operation
    );
    assert!(ex.operation.contains("headline"), "{}", ex.operation);
    assert!(ex.operation.contains("streamUrl"), "{}", ex.operation);
    // no repetition of the common fields inside the fragments
    assert_eq!(
        ex.operation.matches("createdAt").count(),
        1,
        "{}",
        ex.operation
    );
}

#[test]
fn depth_expands_the_fields_that_level_one_leaves_as_markers() {
    let shallow = draft_deep("Query.user", 1);
    assert!(
        shallow.operation.contains("# posts: Post { … }"),
        "{}",
        shallow.operation
    );

    let deep = draft_deep("Query.user", 2);
    graphql_parser::parse_query::<String>(&deep.operation).expect("drafted invalid GraphQL");
    // posts is now a real selection set, and its own object field is the
    // marker at the new boundary
    assert!(deep.operation.contains("posts {"), "{}", deep.operation);
    assert!(deep.operation.contains("title"), "{}", deep.operation);
    assert!(
        deep.operation.contains("# author: User { … }"),
        "{}",
        deep.operation
    );
}

#[test]
fn a_deprecated_target_is_reported_and_still_drafted() {
    // Mutation.deleteUser is @deprecated(reason: "use archiveUser") — flagged,
    // not dropped: silently refusing to draft a field the schema still serves
    // would be its own surprise.
    let ex = draft("Mutation.deleteUser");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(ex.deprecated, ["Mutation.deleteUser (use archiveUser)"]);
    assert!(
        ex.operation.contains("deleteUser(id: $id)"),
        "{}",
        ex.operation
    );
}
