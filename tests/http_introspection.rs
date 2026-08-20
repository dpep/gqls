//! Live-endpoint introspection smoke test.
//!
//! This hits a public GraphQL API over the network, so it is `#[ignore]`d by
//! default — a plain `cargo test` stays offline and hermetic. Run it now and
//! then (or when touching the URL/introspection path) with:
//!
//!     cargo test --test http_introspection -- --ignored
//!
//! Target: the Countries API (https://countries.trevorblades.com) — a small,
//! stable, auth-free public schema.

use gqls::load;
use gqls::load::LoadOptions;
use gqls::model::Kind;

const ENDPOINT: &str = "https://countries.trevorblades.com/";

#[test]
#[ignore = "hits the network; run with --ignored"]
fn introspects_and_searches_a_live_endpoint() {
    // Force a live fetch so the test exercises the network path, not a cache hit.
    let opts = LoadOptions {
        refresh: true,
        ..Default::default()
    };
    let records = load::load(ENDPOINT, &opts).expect("HTTP introspection should succeed");
    assert!(!records.is_empty(), "expected a non-empty schema");

    // Stable fixtures from the Countries schema: the Country object type and the
    // root `country` query field.
    assert!(
        records
            .iter()
            .any(|r| r.kind == Kind::Object && r.name == "Country"),
        "expected a Country object type"
    );
    assert!(
        records.iter().any(|r| r.path == "Query.country"),
        "expected a Query.country root field"
    );

    // The introspected records flow through the same fuzzy scorer as a file
    // schema — a typo'd query still surfaces the type.
    let hits = gqls::search::search("contry", &records, Default::default());
    assert!(
        hits.iter().any(|h| h.record.name == "Country"),
        "fuzzy search should surface Country despite the typo"
    );
}

/// The real proof for `--example`: draft an operation from a live schema, send
/// it back to that same server, and require data in the response. Parsing the
/// draft only shows it's well-formed GraphQL; executing it shows the arguments,
/// variables, and selection set actually agree with the schema they came from.
#[test]
#[ignore = "hits the network; run with --ignored"]
fn drafted_operation_executes_against_the_live_endpoint() {
    let opts = LoadOptions {
        refresh: true,
        ..Default::default()
    };
    let records = load::load(ENDPOINT, &opts).expect("HTTP introspection should succeed");

    let target = records
        .iter()
        .find(|r| r.path == "Query.country")
        .expect("expected a Query.country root field");
    let drafted = gqls::example::build(target, &records, None).expect("drafting should succeed");

    // `country(code: ID!)` — the one thing the caller must supply.
    assert_eq!(
        drafted.variables,
        serde_json::json!({ "code": "<ID!>" }),
        "{}",
        drafted.operation
    );

    let response: serde_json::Value = ureq::post(ENDPOINT)
        .send_json(ureq::json!({
            "query": drafted.operation,
            "variables": { "code": "US" },
        }))
        .expect("the drafted operation should POST cleanly")
        .into_json()
        .expect("a JSON response");

    assert!(
        response.get("errors").is_none(),
        "server rejected the drafted operation: {response}\n{}",
        drafted.operation
    );
    assert_eq!(
        response
            .pointer("/data/country/code")
            .and_then(|v| v.as_str()),
        Some("US"),
        "unexpected payload: {response}"
    );
}
