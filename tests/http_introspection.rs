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
    let hits = gqls::search::search("contry", &records, None, 5);
    assert!(
        hits.iter().any(|h| h.record.name == "Country"),
        "fuzzy search should surface Country despite the typo"
    );
}
