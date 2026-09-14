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

/// A schema with the disease `--via` treats: a global-ID lookup returns an
/// interface every type implements, so every type sits one hop from a root and
/// the shortest route to anything is through `node(id:)`.
const GLOBAL_ID: &str = "\
    type Query { node(id: ID!): Node, repository(name: String!): Repository, viewer: User }\n\
    interface Node { id: ID! }\n\
    type User { login: String! }\n\
    type Repository implements Node { id: ID! issue(number: Int!): Issue issues: IssueConnection! }\n\
    type IssueConnection { nodes: [Issue!]! }\n\
    type Issue implements Node { id: ID! title: String! }\n";

/// Draft `path` from `sdl` along `route`, which is empty for no route.
fn drafted_via(sdl: &str, path: &str, route: &str) -> example::Example {
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{path} missing from the fixture"));
    example::build_via(target, &records, None, route).expect("drafting should succeed")
}

/// Why drafting `path` from `sdl` along `route` can't be done.
fn refused_via(sdl: &str, path: &str, route: &str) -> String {
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("{path} missing from the fixture"));
    example::build_via(target, &records, None, route)
        .expect_err("this route should not draft")
        .to_string()
}

#[test]
fn a_named_route_is_taken_over_the_shortest_one() {
    // The whole point: the walk settles a type at its nearest hop and drops
    // every later edge into it, so the way round through a repository isn't
    // ranked low — without a route named it was never a candidate.
    let shortest = drafted_via(GLOBAL_ID, "Issue.title", "");
    assert_eq!(shortest.via.as_deref(), Some("Query.node"));

    let ex = drafted_via(GLOBAL_ID, "Issue.title", "Query.repository");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(
        ex.via.as_deref(),
        Some("Query.repository > Repository.issue")
    );
    assert!(!ex.operation.contains("node("), "{}", ex.operation);
}

#[test]
fn an_empty_route_is_no_route_rather_than_one_nothing_matches() {
    // The one input that could read either way. It means no route, which is
    // what keeps an empty-string case out of every guard downstream.
    let records = gqls::load::sdl::from_sdl(GLOBAL_ID).expect("should parse");
    let target = records.iter().find(|r| r.path == "Issue.title").unwrap();
    let unrouted = example::build(target, &records, None).expect("drafting should succeed");
    for empty in ["", "   ", " > "] {
        let ex = example::build_via(target, &records, None, empty)
            .unwrap_or_else(|e| panic!("`--via {empty:?}` should draft: {e}"));
        assert_eq!(ex.operation, unrouted.operation);
    }
}

#[test]
fn a_route_of_two_segments_pins_both_hops() {
    // The second segment is what picks the connection: `issue(number:)` gets
    // there a hop sooner, so nothing but saying so reaches `issues`.
    let ex = drafted_via(
        GLOBAL_ID,
        "Issue.title",
        "Query.repository > Repository.issues",
    );
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(
        ex.via.as_deref(),
        Some("Query.repository > Repository.issues > IssueConnection.nodes")
    );
}

#[test]
fn a_route_ending_on_an_abstract_type_still_narrows_to_the_target() {
    // The route says how to get to the union; the draft still has to spell the
    // fragment that makes the target selectable once it's there.
    let sdl = "\
        type Query { node(id: ID!): Node, repository(name: String!): Repository }\n\
        interface Node { id: ID! }\n\
        type Repository implements Node { id: ID! timeline: [Event!]! }\n\
        union Event = Issue | Commit\n\
        type Issue implements Node { id: ID! title: String! }\n\
        type Commit implements Node { id: ID! sha: String! }\n";
    assert_eq!(
        drafted_via(sdl, "Issue.title", "").via.as_deref(),
        Some("Query.node"),
        "the fixture should have the disease"
    );

    let ex = drafted_via(sdl, "Issue.title", "Query.repository > Repository.timeline");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert!(ex.operation.contains("timeline {"), "{}", ex.operation);
    assert!(ex.operation.contains("... on Issue {"), "{}", ex.operation);
}

#[test]
fn a_route_segment_naming_no_field_says_which_segment() {
    // Every way a route can fail used to come out as "isn't reachable from a
    // root field — nothing returns Issue", which is false twice over.
    let err = refused_via(
        GLOBAL_ID,
        "Issue.title",
        "Query.repository > Repository.issuez",
    );
    assert!(err.contains("hop 2"), "{err}");
    assert!(err.contains("Repository.issuez"), "{err}");
    // and what did work, so the reader knows where to look
    assert!(err.contains("Query.repository"), "{err}");
    assert!(!err.contains("reachable from a root field"), "{err}");
}

#[test]
fn a_route_leading_nowhere_near_the_target_says_that_and_not_that_nothing_does() {
    let err = refused_via(GLOBAL_ID, "Issue.title", "Query.viewer");
    assert!(err.contains("Query.viewer"), "{err}");
    assert!(err.contains("--via"), "{err}");
    assert!(!err.contains("nothing returns Issue"), "{err}");
}

#[test]
fn a_target_the_route_puts_past_the_cap_says_the_route_is_why() {
    // What's past the cap here is the way round, not the target: the same
    // field drafts in one hop without the route.
    let sdl = "\
        type Query { node(id: ID!): Node, a: A }\n\
        interface Node { id: ID! }\n\
        type A { b: B }\n\
        type B { c: C }\n\
        type C { d: D }\n\
        type D { e: E }\n\
        type E { f: F }\n\
        type F { g: G }\n\
        type G implements Node { id: ID! deep: String! }\n";
    assert_eq!(
        drafted_via(sdl, "G.deep", "").via.as_deref(),
        Some("Query.node")
    );

    let err = refused_via(sdl, "G.deep", "Query.a");
    assert!(err.contains("7 hops"), "{err}");
    assert!(err.contains("6-hop cap"), "{err}");
    assert!(err.contains("Query.a"), "{err}");
}

#[test]
fn a_route_is_honoured_for_an_input_a_root_field_takes() {
    // The one chain that never enters the walk, and so the one edge where a
    // route could be applied everywhere else and quietly ignored here.
    let sdl = "\
        type Query { ping: String }\n\
        type Mutation { star(input: StarInput!): String, unstar(input: StarInput!): String }\n\
        input StarInput { id: ID! }\n";
    assert_eq!(
        drafted_via(sdl, "StarInput", "").via.as_deref(),
        Some("Mutation.star(input:)")
    );

    // named with the `(arg:)` the path carries, so a printed path goes back in
    let ex = drafted_via(sdl, "StarInput", "Mutation.unstar(input:)");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert_eq!(ex.via.as_deref(), Some("Mutation.unstar(input:)"));

    let err = refused_via(sdl, "StarInput", "Query.ping");
    assert!(err.contains("Query.ping"), "{err}");
    assert!(err.contains("StarInput"), "{err}");
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
            "input": { "name": "<String!>", "email": "<String!>", "role": "<Role = MEMBER>" }
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
    // The nested one is named by the whole path to it, not by the field alone:
    // a consumer four hops out is no answer to "where does this go" unless the
    // way in comes with it.
    assert_eq!(ex.alternatives, ["Query.users > User.posts(filter:)"]);
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
fn a_defaulted_input_field_says_so_in_the_skeleton() {
    // `"<OrderDirection!>"` reads as mandatory, but the schema fills it in.
    let ex = draft("PostOrder");
    assert_eq!(
        ex.variables["orderBy"][0]["direction"],
        "<OrderDirection! = ASC>"
    );
    assert_eq!(ex.variables["orderBy"][0]["field"], "<PostOrderField!>");
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

    assert_eq!(ex.via.as_deref(), Some("Query.posts > Post.search(where:)"));
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
    let err = example::build(target, &records, None)
        .expect_err("nothing takes an Orphan, and no input holds one either");
    let err = err.to_string();
    assert!(err.contains("Orphan"), "{err}");
    assert!(err.contains("holds one is taken"), "{err}");
}

#[test]
fn a_field_reached_only_through_a_union_is_drafted_through_it() {
    // Nothing returns a Pet, but every member of Animal is one, so
    // `... on Pet` is how the field is selected — and it is a query people
    // really write, not a consolation path.
    let sdl = "\
        type Query { pets: [Animal!]! }\n\
        union Animal = Cat | Dog\n\
        interface Pet { nickname: String! }\n\
        type Cat implements Pet { nickname: String! livesLeft: Int! }\n\
        type Dog implements Pet { nickname: String! breed: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Pet.nickname").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Query.pets"));
    assert_eq!(
        ex.operation,
        "query Nickname {\n  \
           pets {\n    \
             __typename\n    \
             ... on Pet {\n      \
               nickname\n    \
             }\n  \
           }\n\
         }\n"
    );
}

#[test]
fn a_root_returning_an_implementor_selects_the_field_without_a_fragment() {
    // Nothing returns a Commentable either, but Query.posts returns a Post,
    // which implements it — the field is already on that type.
    let ex = draft("Commentable.comments");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Query.posts"));
    assert!(!ex.operation.contains("... on"), "{}", ex.operation);
    assert!(ex.operation.contains("posts {"), "{}", ex.operation);
    assert!(ex.operation.contains("comments {"), "{}", ex.operation);
}

#[test]
fn a_type_drafts_the_root_that_fetches_one() {
    // Asking about a type is asking how to fetch one, and the only path to a
    // Cat is the union — so the draft is the root, narrowed to it.
    let sdl = "\
        type Query { pets: [Animal!]! }\n\
        union Animal = Cat | Dog\n\
        type Cat { nickname: String! livesLeft: Int! }\n\
        type Dog { nickname: String! breed: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Cat").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Query.pets"));
    assert_eq!(
        ex.operation,
        "query Cat {\n  \
           pets {\n    \
             __typename\n    \
             ... on Cat {\n      \
               nickname\n      \
               livesLeft\n    \
             }\n  \
           }\n\
         }\n"
    );
}

#[test]
fn a_type_a_root_returns_outright_needs_no_fragment() {
    let ex = draft("Node");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(ex.via.as_deref(), Some("Query.node"));
    assert!(
        ex.operation.starts_with("query Node($id: ID!) {"),
        "{}",
        ex.operation
    );
    // narrowed to nothing — but its implementors still get their fragments
    assert!(!ex.operation.contains("... on Node"), "{}", ex.operation);
    assert!(ex.operation.contains("... on User {"), "{}", ex.operation);
}

#[test]
fn something_no_operation_can_select_points_at_what_returns_one() {
    let records = records();
    for (path, hint) in [("Role", "--returns Role"), ("Role.ADMIN", "--returns Role")] {
        let target = records.iter().find(|r| r.path == path).unwrap();
        let err = example::build(target, &records, None)
            .expect_err("an enum can't be selected")
            .to_string();
        assert!(err.contains(hint), "{err}");
    }
    // A directive is returned by nothing, so it gets no pointer at all.
    let target = records.iter().find(|r| r.path == "@auth").unwrap();
    let err = example::build(target, &records, None)
        .expect_err("a directive can't be selected")
        .to_string();
    assert!(!err.contains("--returns"), "{err}");
}

#[test]
fn a_deprecated_object_valued_field_keeps_its_brace_out_of_the_comment() {
    // The note is a `#` comment, so everything after it on the line is comment
    // too. Appending it before the `{` left the document unbalanced — and the
    // errors convention is always expanded, so this fired at the default depth.
    let sdl = "\
        type Query { ping: String }\n\
        type Mutation { save(id: ID!): SavePayload }\n\
        type SavePayload { errors: [UserError!]! @deprecated(reason: \"use problems\") ok: Boolean! }\n\
        type UserError { message: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Mutation.save").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(
        ex.operation
            .contains("errors {  # deprecated: use problems"),
        "{}",
        ex.operation
    );
}

#[test]
fn a_multi_line_deprecation_reason_stays_inside_its_comment() {
    // The same leak as the brace, from the other side: a `#` comment ends at
    // the newline, so only the reason's first line stayed commented and the
    // rest landed in the selection set, where a server read it as fields. It
    // parses either way, which is why this asserts on the shape and not on
    // `parse_query`.
    let sdl = "\
        type Query { team: Team @deprecated(reason: \"\"\"\n\
        \x20 Querying a Team at the root is discouraged.\n\
        \x20 Ref T12456.\n\
        \"\"\") }\n\
        type Team { invitations: Invitations @deprecated(reason: \"\"\"\n\
        \x20 Use a generic connection.\n\
        \x20 Interim until the generic type exists.\n\
        \"\"\") }\n\
        type Invitations { totalCount: Int }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.team").unwrap();
    let ex = example::build(target, &records, Some(2)).expect("drafting should succeed");

    // Inline: the whole reason, on the one line the note opened.
    assert!(
        ex.operation.contains(
            "invitations {  # deprecated: Use a generic connection. \
             Interim until the generic type exists.\n"
        ),
        "{}",
        ex.operation
    );
    // And the same reason carried out to the caller, which prints it as one
    // status line.
    assert_eq!(
        ex.deprecated[0],
        "Query.team (Querying a Team at the root is discouraged. Ref T12456.)"
    );
}

#[test]
fn a_selection_with_no_leaf_in_it_says_so() {
    // A Relay connection has nothing but object-valued fields, so one level
    // draws markers and a `__typename` — a query that runs and fetches
    // nothing, with no hint that `--depth` is what fills it in.
    let sdl = "\
        type Query { characters: Characters }\n\
        type Characters { info: Info results: [Character] }\n\
        type Info { count: Int }\n\
        type Character { name: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records
        .iter()
        .find(|r| r.path == "Query.characters")
        .unwrap();

    assert!(example::build(target, &records, None).unwrap().no_leaves);
    // One level further reaches real fields, and there's nothing left to say.
    let deeper = example::build(target, &records, Some(2)).unwrap();
    assert!(!deeper.no_leaves, "{}", deeper.operation);
    // Depth zero hides leaves rather than finding none, so it isn't the same
    // claim — an input target draws it deliberately.
    assert!(!example::build(target, &records, Some(0)).unwrap().no_leaves);
}

#[test]
fn a_selection_only_arguments_could_fill_does_not_point_at_depth() {
    // The other way a level draws no leaf: a namespace container whose fields
    // every one needs arguments. It looks like the connection above — markers
    // and a `__typename` — but `--depth` can't select through a field that
    // needs arguments, so the note sent the reader after a flag that does
    // nothing. The markers say "needs arguments" for themselves.
    let sdl = "\
        type Query { ping: String }\n\
        type Mutation { card: CardMutations }\n\
        type CardMutations { activate(id: ID!): Boolean! reorder(id: ID!): Boolean! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Mutation.card").unwrap();

    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
    assert!(
        ex.operation.contains("— needs arguments"),
        "{}",
        ex.operation
    );
    assert!(!ex.no_leaves, "{}", ex.operation);
    // …and deeper doesn't change that, which is the whole point.
    assert!(!example::build(target, &records, Some(3)).unwrap().no_leaves);
}

#[test]
fn an_implementor_that_only_adds_object_fields_still_appears() {
    // Its additions are all object-valued, so it has nothing but markers —
    // and dropping every marker inside an interface's fragments dropped the
    // implementor with them, leaving nothing to say it exists.
    let sdl = "\
        type Query { animals: [Animal!]! }\n\
        interface Animal { id: ID! }\n\
        type Dog implements Animal { id: ID! toy: Toy }\n\
        type Cat implements Animal { id: ID! lives: Int }\n\
        type Toy { name: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.animals").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(ex.operation.contains("... on Dog {"), "{}", ex.operation);
    assert!(ex.operation.contains("# toy: Toy"), "{}", ex.operation);
    // the interface's own field is selected once, not repeated per implementor
    assert_eq!(
        ex.operation.matches("\n    id\n").count(),
        1,
        "{}",
        ex.operation
    );
}

#[test]
fn a_field_the_interface_already_selected_is_dropped_whole() {
    // Dropping only the opening line of a `field { … }` the interface had
    // already selected left its body and closing brace orphaned, and the
    // operation didn't parse. The deprecated case is the one that survives a
    // fix testing the line's last character: the note sits past the brace.
    let sdl = "\
        type Query { things: [Thing!]! }\n\
        interface Thing { owner: Owner @deprecated(reason: \"use holder\") }\n\
        type Widget implements Thing { owner: Owner @deprecated(reason: \"use holder\") size: Int }\n\
        type Owner { name: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.things").unwrap();
    let ex = example::build(target, &records, Some(2)).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    // the interface selects it once, and the implementor adds only its own
    assert_eq!(
        ex.operation.matches("owner {").count(),
        1,
        "{}",
        ex.operation
    );
    assert!(ex.operation.contains("... on Widget {"), "{}", ex.operation);
    assert!(ex.operation.contains("size"), "{}", ex.operation);
}

#[test]
fn fields_that_clash_across_members_are_aliased_apart() {
    // `email: String` beside `email: String!` under one response name is a
    // shape the spec forbids, whatever a given server currently tolerates.
    let sdl = "\
        type Query { owner: Owner }\n\
        union Owner = Org | Person\n\
        type Org { email: String name: String! }\n\
        type Person { email: String! name: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.owner").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert!(ex.operation.contains("orgEmail: email"), "{}", ex.operation);
    assert!(
        ex.operation.contains("personEmail: email"),
        "{}",
        ex.operation
    );
    // the name that means the same thing in both is left alone
    assert_eq!(
        ex.operation.matches(": name").count(),
        0,
        "{}",
        ex.operation
    );
}

#[test]
fn a_draft_says_what_its_documented_arguments_are_for() {
    // The signature says what to pass. What passing it *does* is the half a
    // draft can't show by shape — `at: DateTime` never says that omitting it
    // means now.
    let ex = draft("Mutation.publishPost");
    let at = ex
        .arguments
        .iter()
        .find(|a| a.name == "at")
        .unwrap_or_else(|| panic!("{:?}", ex.arguments));
    assert!(at.description.starts_with("When to publish"), "{at:?}");
    // an argument the schema doesn't document isn't invented
    assert!(
        !ex.arguments.iter().any(|a| a.name == "id"),
        "{:?}",
        ex.arguments
    );
}

#[test]
fn a_target_many_hops_from_a_root_is_reached_through_the_whole_chain() {
    // The namespaced-root pattern: the root hands back a container and the
    // fields people actually want hang several levels off it. Refusing
    // anything past one hop refused most of a schema shaped like this.
    let sdl = "\
        type Query { payroll: PayrollQueries }\n\
        type PayrollQueries { company: Company }\n\
        type Company { employee(id: ID!): Employee }\n\
        type Employee { badge: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Employee.badge").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.via.as_deref(),
        Some("Query.payroll > PayrollQueries.company > Company.employee")
    );
    assert_eq!(
        ex.operation,
        "query Badge($id: ID!) {\n  \
           payroll {\n    \
             company {\n      \
               employee(id: $id) {\n        \
                 badge\n      \
               }\n    \
             }\n  \
           }\n\
         }\n"
    );
}

#[test]
fn a_cycle_in_the_type_graph_closes_rather_than_looping() {
    // `User.posts` beside `Post.author` is the shape every schema has. The
    // walk settles a type the first time it sees one, so the chain is the
    // shortest way in and never passes back through a type it already used.
    let sdl = "\
        type Query { user: User }\n\
        type User { posts: [Post!]! }\n\
        type Post { author: User! comments: [Comment!]! }\n\
        type Comment { body: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Comment.body").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.via.as_deref(),
        Some("Query.user > User.posts > Post.comments")
    );
    // the way back round is never taken, however many times it's offered
    assert!(!ex.operation.contains("author"), "{}", ex.operation);
}

#[test]
fn a_target_past_the_hop_cap_is_refused_with_the_distance_it_sits_at() {
    // Seven types deep, so the last one is out of reach and the one before it
    // is the last that isn't — the cap has to be a boundary, not a wall.
    let sdl = "\
        type Query { a: A }\n\
        type A { b: B }\n\
        type B { c: C }\n\
        type C { d: D }\n\
        type D { e: E }\n\
        type E { f: F }\n\
        type F { g: G }\n\
        type G { deep: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");

    let far = records.iter().find(|r| r.path == "G.deep").unwrap();
    let err = example::build(far, &records, None)
        .expect_err("seven hops is past the cap")
        .to_string();
    assert!(err.contains("7 hops"), "{err}");
    assert!(err.contains("6-hop cap"), "{err}");

    let near = records.iter().find(|r| r.path == "F.g").unwrap();
    let ex = example::build(near, &records, None).expect("six hops is still drafted");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");
}

#[test]
fn a_deep_consumer_is_reached_through_the_chain_that_carries_the_input() {
    // The other edge of the same walk: an input is reached by the path to the
    // field taking it, however far out that field sits.
    let sdl = "\
        type Query { admin: AdminQueries }\n\
        type AdminQueries { users: UserQueries }\n\
        type UserQueries { search(where: UserFilter): [User!]! }\n\
        type User { id: ID! }\n\
        input UserFilter { term: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "UserFilter").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.via.as_deref(),
        Some("Query.admin > AdminQueries.users > UserQueries.search(where:)")
    );
    assert!(
        ex.operation.contains("search(where: $where)"),
        "{}",
        ex.operation
    );
    assert!(ex.variables["where"].is_object(), "{:?}", ex.variables);
}

#[test]
fn every_hop_that_needs_a_fragment_gets_one() {
    // A chain through two abstract types needs narrowing twice, at different
    // levels of the same operation — the single fragment position a one-hop
    // draft could get away with silently dropped the outer one.
    let sdl = "\
        type Query { feed: Feed }\n\
        union Feed = Article\n\
        type Article { body: Block }\n\
        union Block = Quote\n\
        type Quote { text: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Quote.text").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.operation,
        "query Text {\n  \
           feed {\n    \
             __typename\n    \
             ... on Article {\n      \
               body {\n        \
                 __typename\n        \
                 ... on Quote {\n          \
                   text\n        \
                 }\n      \
               }\n    \
             }\n  \
           }\n\
         }\n"
    );
}

#[test]
fn an_argument_name_repeated_along_a_chain_gets_a_variable_of_its_own() {
    // Three `node(id:)` hops: qualifying by field name alone gave the inner
    // two the same `$nodeId`, which is a duplicate variable the server
    // rejects. Every hop has to end up with a name nothing else took.
    let sdl = "\
        type Query { node(id: ID!): Level1 }\n\
        type Level1 { node(id: ID!): Level2 }\n\
        type Level2 { node(id: ID!): Level3 }\n\
        type Level3 { name: String }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Level3.name").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    assert_eq!(
        ex.variables,
        serde_json::json!({ "id": "<ID!>", "nodeId": "<ID!>", "nodeId2": "<ID!>" })
    );
    assert!(
        ex.operation.contains("node(id: $nodeId2)"),
        "{}",
        ex.operation
    );
}

#[test]
fn an_input_only_another_input_holds_is_drafted_through_that_one() {
    // Nothing takes an Address, so there was no operation to draft — while the
    // input that holds it is taken, and the draft showing where an Address goes
    // is that one's, with the shape nested inside where you'd paste it.
    let sdl = "\
        type Query { ping: String }\n\
        type Mutation { validate(input: Validation!): Boolean! }\n\
        input Validation { address: Address! }\n\
        input Address { city: String! zip: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Address").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    // the operation passes the input that is actually an argument
    assert_eq!(ex.through.as_deref(), Some("Validation"));
    assert!(
        ex.operation.contains("validate(input: $input)"),
        "{}",
        ex.operation
    );
    // and the thing asked about is expanded where it sits inside it
    assert_eq!(
        ex.variables,
        serde_json::json!({ "input": { "address": { "city": "<String!>", "zip": "<String!>" } } })
    );
    // one holder, so nothing to choose between
    assert!(ex.alternatives.is_empty(), "{:?}", ex.alternatives);
}

#[test]
fn every_input_that_holds_the_target_is_offered() {
    // Two inputs hold an Address and both are taken, so "where does this go"
    // has two answers. The nearest-holder rule still decides what gets drafted,
    // but hiding the other would pass a semantically different operation off as
    // the only one — the same property the direct-take path already has.
    let sdl = "\
        type Query { ping: String }\n\
        type Mutation {\n\
          bill(input: Billing!): Boolean!\n\
          ship(input: Shipping!): Boolean!\n\
        }\n\
        input Billing { address: Address! }\n\
        input Shipping { address: Address! }\n\
        input Address { city: String! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Address").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    // Each path names the input its argument carries: the alternative isn't
    // just another field, it's another thing to pass.
    assert_eq!(
        ex.paths(),
        [
            "Mutation.bill(input: Billing)",
            "Mutation.ship(input: Shipping)"
        ]
    );
    assert_eq!(ex.through.as_deref(), Some("Billing"));
}

#[test]
fn an_input_held_deeper_than_the_skeleton_reaches_is_refused() {
    // The holder walk used to be uncapped while the skeleton stops at six
    // levels, so a draft announced "L9 is passed inside L1" over variables
    // whose deepest mention was L7. Refusing names the distance instead.
    let mut sdl = String::from(
        "type Query { ping: String }\n\
         type Mutation { outer(input: L1!): Boolean! }\n",
    );
    for level in 1..9 {
        sdl.push_str(&format!("input L{level} {{ next: L{}! }}\n", level + 1));
    }
    sdl.push_str("input L9 { city: String! }\n");
    let records = gqls::load::sdl::from_sdl(&sdl).expect("should parse");

    let target = records.iter().find(|r| r.path == "L9").unwrap();
    let err = example::build(target, &records, None)
        .expect_err("eight levels deep is past the cap")
        .to_string();
    assert!(err.contains("8 levels inside"), "{err}");

    // The last level the skeleton still names is still drafted — the cap is
    // the skeleton's own, not a fresh number.
    let target = records.iter().find(|r| r.path == "L7").unwrap();
    let ex = example::build(target, &records, None).expect("six levels still drafts");
    assert!(
        ex.variables.to_string().contains("<L7!>"),
        "{}",
        ex.variables
    );
}

#[test]
fn an_errors_cycle_ends_the_selection_instead_of_the_process() {
    // The payload/errors convention expands at the last level too, and used to
    // do it unconditionally — so `Payload.errors -> UserError.errors -> Payload`
    // recursed until the stack ran out, on default flags and a legal schema.
    let sdl = "\
        type Query { go: Payload }\n\
        type Payload { ok: Boolean  errors: [UserError!]! }\n\
        type UserError { message: String!  errors: [Payload!]! }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.go").unwrap();
    let ex = example::build(target, &records, None).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&ex.operation).expect("drafted invalid GraphQL");

    // The convention still has to fire: a draft without its errors block is
    // the thing the convention exists to prevent.
    assert!(ex.operation.contains("errors {"), "{}", ex.operation);
    assert!(ex.operation.contains("message"), "{}", ex.operation);
    // …and the hop back into a type already open above is a marker, not a
    // level — there is no depth left to spend on it.
    assert!(
        ex.operation.contains("# errors: Payload { … }"),
        "{}",
        ex.operation
    );
}

#[test]
fn a_recursive_type_is_drafted_to_the_cap_not_to_the_depth_asked_for() {
    // `--depth` took any usize. A self-referential type turned a typo into a
    // stack overflow, and a merely large one into tens of millions of lines.
    let sdl = "type Query { widget: Widget }\ntype Widget { name: String  self: Widget }\n";
    let records = gqls::load::sdl::from_sdl(sdl).expect("should parse");
    let target = records.iter().find(|r| r.path == "Query.widget").unwrap();

    let asked = example::build(target, &records, Some(100_000)).expect("drafting should succeed");
    graphql_parser::parse_query::<String>(&asked.operation).expect("drafted invalid GraphQL");

    let capped =
        example::build(target, &records, Some(example::MAX_DEPTH)).expect("the cap itself drafts");
    assert_eq!(asked.operation, capped.operation);
    // The last level has no depth left, so it leaves the marker.
    assert_eq!(
        asked.operation.matches("self {").count(),
        example::MAX_DEPTH - 1,
        "{}",
        asked.operation
    );
}
