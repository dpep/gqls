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

/// Draft at the default depth for the target's kind.
fn draft(path: &str) -> example::Example {
    draft_at(path, None)
}

fn draft_deep(path: &str, depth: usize) -> example::Example {
    draft_at(path, Some(depth))
}

fn draft_at(path: &str, depth: Option<usize>) -> example::Example {
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
fn a_variable_skeleton_shows_the_input_rather_than_naming_it() {
    // `"<CreateUserInput!>"` only restated the signature, so the shape had to
    // be printed a second time as SDL. Expanding it makes the block you paste
    // the block that shows the shape, in the schema's own field order.
    let ex = draft("Mutation.createUser");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.variables,
        serde_json::json!({
            "input": { "name": "<String!>", "email": "<String!>", "role": "<Role>" }
        })
    );
    // the key alone stops saying what type it is, so the heading says it
    assert_eq!(ex.variable_types, ["input: CreateUserInput!"]);

    // Role is reached only through CreateUserInput.role — JSON can't hold a
    // choice, so the values are listed beside the block, not inside it
    assert_eq!(ex.enums, ["Role = ADMIN | MEMBER | GUEST | OWNER"]);
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
    let ex = example::build(target, &records, None).expect("drafting should succeed");
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

#[test]
fn drafts_an_input_object_through_the_field_that_takes_it() {
    // CreateUserInput isn't callable — nothing returns it and no operation
    // names it. What it answers is "where does this go", and the answer is the
    // mutation whose argument it is.
    let ex = draft("CreateUserInput");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Mutation.createUser(input:)"));
    assert!(
        ex.operation
            .starts_with("mutation CreateUser($input: CreateUserInput!) {"),
        "{}",
        ex.operation
    );
    // the skeleton, not a bare placeholder — see
    // a_variable_skeleton_shows_the_input_rather_than_naming_it
    assert_eq!(ex.variables["input"]["email"], "<String!>");
}

#[test]
fn supplies_the_named_input_even_where_the_schema_calls_it_optional() {
    // Query.posts(filter: PostFilter) is nullable, so the usual rule files it
    // under "optional arguments" and leaves it out of the operation entirely —
    // which would draft a query that never mentions the type asked about.
    let ex = draft("PostFilter");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(
        ex.operation.contains("posts(filter: $filter)"),
        "{}",
        ex.operation
    );
    // and it's a real variable, not merely mentioned
    assert!(ex.variables["filter"].is_object(), "{:?}", ex.variables);
    // the other optional argument is still left out, as it always was
    assert!(
        ex.optional.iter().any(|o| o.starts_with("posts(orderBy:")),
        "{:?}",
        ex.optional
    );
}

#[test]
fn an_input_taken_in_two_places_offers_both() {
    // Query.posts and User.posts both take a PostFilter. The root wins — it's
    // one hop — and the nested one is offered rather than dropped.
    let ex = draft("PostFilter");
    assert_eq!(ex.via.as_deref(), Some("Query.posts(filter:)"));
    assert_eq!(ex.alternatives, ["User.posts(filter:)"]);
}

#[test]
fn an_input_field_rides_on_the_input_object_that_holds_it() {
    // CreateUserInput.email can't be passed on its own; the operation that can
    // carry it is the one taking the whole input.
    let ex = draft("CreateUserInput.email");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(ex.via.as_deref(), Some("Mutation.createUser(input:)"));
    assert_eq!(
        ex.description.as_deref(),
        Some("Must be unique across the account; a verification mail is sent here.")
    );
}

#[test]
fn an_input_draft_keeps_the_input_in_view_rather_than_the_payload() {
    // The question is where the input goes, so the reply gets the barest
    // selection a server will accept — eight lines of leaf fields buried the
    // one line the draft exists to show. `--depth` asks for the payload back.
    let bare = draft("PostFilter");
    graphql_parser::parse_query::<String>(&bare.operation).expect("drafted invalid GraphQL");
    assert!(bare.operation.contains("__typename"), "{}", bare.operation);
    assert!(
        !bare.operation.contains("publishedAt"),
        "{}",
        bare.operation
    );

    let full = draft_deep("PostFilter", 1);
    graphql_parser::parse_query::<String>(&full.operation).expect("drafted invalid GraphQL");
    assert!(full.operation.contains("publishedAt"), "{}", full.operation);

    // a field target is untouched — one level, as it always was
    assert!(draft("Query.posts").operation.contains("publishedAt"));
}

#[test]
fn an_input_draft_expands_only_what_that_input_reaches() {
    // Query.posts also takes an orderBy, whose PostOrder / PostOrderField /
    // OrderDirection have nothing to do with the type asked about — and it
    // isn't even filled in, since it carries a schema default.
    let ex = draft("PostFilter");
    assert_eq!(
        ex.variables,
        serde_json::json!({
            "filter": {
                "authorId": "<ID>",
                "tags": ["<String!>"],
                "publishedAfter": "<DateTime>",
                // the self-reference closes here rather than recursing
                "not": "<PostFilter>",
            }
        })
    );
    assert!(ex.enums.is_empty(), "{:?}", ex.enums);
}

#[test]
fn a_nested_consumer_is_wrapped_in_a_root_and_an_unreachable_one_is_dropped() {
    // Two fields take a Filter. `Orphan.search` is on a type no root returns,
    // so it isn't a path you could call; `Post.search` is reached through the
    // root that returns a Post.
    let sdl = "\
        type Query { posts: [Post!]! }\n\
        type Post { search(where: Filter): [Post!]! }\n\
        type Orphan { search(where: Filter): [Post!]! }\n\
        input Filter { term: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Filter").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Post.search(where:)"));
    assert!(ex.alternatives.is_empty(), "{:?}", ex.alternatives);
    assert!(ex.operation.contains("posts {"), "{}", ex.operation);
    assert!(
        ex.operation.contains("search(where: $where)"),
        "{}",
        ex.operation
    );
}

#[test]
fn an_input_nothing_takes_says_so_rather_than_inventing_a_path() {
    let sdl = "type Query { ping: String }\ninput Orphan { a: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Orphan").unwrap();
    let err = example::build(target, &records, None).expect_err("nothing takes an Orphan");
    assert!(err.to_string().contains("Orphan"), "{err}");
}
