//! Fetching a live schema: what is retried, what isn't, and what a failure
//! says. Hermetic — the endpoint is in this process (`common::endpoint`).

mod common;
#[path = "common/endpoint.rs"]
mod endpoint;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use endpoint::{Endpoint, Reply};

fn cache_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gqls-fetch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp cache dir");
    dir
}

fn gqls(url: &str, cache: &Path, args: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg(url)
        .args(args)
        .env("XDG_CACHE_HOME", cache)
        .env("GQLS_INTROSPECT_TTL", "3600")
        .output()
        .expect("gqls should be runnable")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn a_transient_failure_is_retried_and_says_so_under_v() {
    for first in [
        Reply::Status(503, "busy", &[]),
        Reply::Status(429, "slow down", &["Retry-After: 0"]),
        Reply::Close,
    ] {
        let ep = Endpoint::start(&[first, Reply::Schema("widget")]);
        let cache = cache_dir("retry");
        let out = gqls(&ep.url, &cache, &["widget", "-v"]);
        let err = text(&out.stderr);
        assert!(out.status.success(), "{err}");
        assert!(text(&out.stdout).contains("widget"));
        assert!(err.contains("retrying"), "{err}");
        assert_eq!(ep.hits(), 2);
    }
}

#[test]
fn a_refusal_is_not_retried() {
    // An auth failure or a GraphQL error body won't change on a second ask.
    for reply in [
        Reply::Status(401, r#"{"errors":[{"message":"bad token"}]}"#, &[]),
        Reply::Status(
            200,
            r#"{"errors":[{"message":"introspection is disabled"}]}"#,
            &[],
        ),
    ] {
        let ep = Endpoint::start(&[reply]);
        let out = gqls(&ep.url, &cache_dir("refusal"), &["widget"]);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert_eq!(ep.hits(), 1, "{}", text(&out.stderr));
    }
}

#[test]
fn a_persistent_failure_gives_up_after_a_bounded_number_of_tries() {
    let ep = Endpoint::start(&[Reply::Status(502, "bad gateway", &[])]);
    let out = gqls(&ep.url, &cache_dir("persistent"), &["widget"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(ep.hits(), 3);
}

#[test]
fn a_failure_says_what_happened_once() {
    // ureq's own text repeats the URL and its error kind:
    // `URL: introspecting URL: Network Error: Network Error: …`.
    let ep = Endpoint::start(&[Reply::Status(
        401,
        r#"{"errors":[{"message":"bad token"}]}"#,
        &[],
    )]);
    let out = gqls(&ep.url, &cache_dir("text"), &["widget"]);
    let err = text(&out.stderr);
    assert_eq!(err.matches(&ep.url).count(), 1, "{err}");
    assert!(err.contains("401"), "{err}");
    assert!(err.contains("bad token"), "{err}");

    // nothing listening: connection refused, said once
    let url = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        format!("http://{}/graphql", l.local_addr().expect("an address"))
    };
    let out = gqls(&url, &cache_dir("refused"), &["widget"]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(err.matches(&url).count(), 1, "{err}");
    assert!(err.contains("couldn't connect"), "{err}");
}

#[test]
fn a_slow_fetch_says_it_is_still_waiting() {
    let ep = Endpoint::start(&[Reply::Slow(
        std::time::Duration::from_millis(2500),
        "widget",
    )]);
    let out = gqls(&ep.url, &cache_dir("slow"), &["widget"]);
    let err = text(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("still waiting on"), "{err}");

    // and a prompt one says nothing of the kind
    let ep = Endpoint::start(&[Reply::Schema("widget")]);
    let out = gqls(&ep.url, &cache_dir("prompt"), &["widget"]);
    assert!(!text(&out.stderr).contains("still waiting"));
}

fn ndjson(out: &Output) -> Vec<serde_json::Value> {
    text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

#[test]
fn an_answer_from_a_cached_copy_says_how_old_it_is() {
    let ep = Endpoint::start(&[Reply::Schema("widget")]);
    let cache = cache_dir("age");

    // fetched live: nothing to disclose
    let out = gqls(&ep.url, &cache, &["widget", "-J"]);
    assert!(
        !text(&out.stderr).contains("cached"),
        "{}",
        text(&out.stderr)
    );
    assert!(ndjson(&out)[0].get("source").is_none());

    // from the cache: the answer and the miss both carry their age
    for query in ["widget", "zzzz"] {
        let out = gqls(&ep.url, &cache, &[query, "-J"]);
        let err = text(&out.stderr);
        assert!(out.status.success(), "{err}");
        assert!(err.contains("cached") && err.contains("--refresh"), "{err}");
        let rows = ndjson(&out);
        let source = &rows[0]["source"];
        assert_eq!(source["url"], ep.url.as_str(), "{query}: {rows:?}");
        assert_eq!(source["cached"], true, "{query}: {rows:?}");
        assert!(source["age_secs"].is_u64(), "{query}: {rows:?}");
    }
    assert_eq!(ep.hits(), 1);
}

/// Age every cached introspection response by `secs`, as if fetched that long
/// ago.
fn backdate(cache: &Path, secs: u64) {
    let dir = cache.join("gqls/introspect");
    for e in std::fs::read_dir(&dir).expect("a response cache").flatten() {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(e.path())
            .and_then(|f| f.set_modified(when))
            .expect("backdating");
    }
}

#[test]
fn a_failed_refetch_answers_from_the_stale_copy_and_says_so() {
    for failure in [
        Reply::Status(500, "boom", &[]),
        Reply::Status(
            200,
            r#"{"errors":[{"message":"introspection is disabled"}]}"#,
            &[],
        ),
        Reply::Close,
    ] {
        let ep = Endpoint::start(&[Reply::Schema("widget")]);
        let cache = cache_dir("stale");
        assert!(gqls(&ep.url, &cache, &["widget"]).status.success());
        backdate(&cache, 2 * 3600);
        ep.set(&[failure]);

        // said even under -q: the answer may be out of date, and the fetch failed
        let out = gqls(&ep.url, &cache, &["widget", "-J", "-q"]);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{err}");
        assert!(err.contains("cached 2h ago"), "{err}");
        let source = &ndjson(&out)[0]["source"];
        assert_eq!(source["stale"], true, "{source}");
        assert_eq!(source["cached"], true, "{source}");
        assert!(source["age_secs"].as_u64().unwrap_or(0) >= 7200, "{source}");
        assert!(source["error"].is_string(), "{source}");

        // --refresh asks for a refetch; failing it, the copy still answers
        let out = gqls(&ep.url, &cache, &["widget", "--refresh"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
}

#[test]
fn a_refused_credential_is_never_answered_from_a_copy() {
    // A revoked token stops working when it's revoked, not when the copy
    // fetched with it ages out.
    let ep = Endpoint::start(&[Reply::Schema("widget")]);
    let cache = cache_dir("revoked");
    assert!(gqls(&ep.url, &cache, &["widget"]).status.success());
    backdate(&cache, 2 * 3600);
    ep.set(&[Reply::Status(
        401,
        r#"{"errors":[{"message":"bad token"}]}"#,
        &[],
    )]);
    let out = gqls(&ep.url, &cache, &["widget"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(text(&out.stdout).is_empty());
}
