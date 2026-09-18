//! The introspection response cache, end to end against a mock endpoint that
//! answers differently per credential.
//!
//! Hermetic: the server is a `TcpListener` in this process, the cache lives in
//! a temp dir handed to the binary via `XDG_CACHE_HOME`, and `GQLS_INTROSPECT_TTL`
//! opts loopback back into caching (it is TTL-0 by default, so a plain
//! `127.0.0.1` endpoint would never cache and the test would prove nothing).
//!
//! The assertion that matters is what the *server* saw: a cache keyed on the URL
//! alone replays the first caller's schema to every later one, and the only
//! direct evidence of that is the requests it never received.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// A one-field schema. The mock serves a different one per credential, so which
/// field comes back says which caller's schema was served.
fn schema_body(field: &str) -> String {
    serde_json::json!({
        "data": { "__schema": {
            "queryType": { "name": "Query" },
            "types": [{
                "kind": "OBJECT",
                "name": "Query",
                "fields": [{
                    "name": field,
                    "description": "TENANT DATA",
                    "args": [],
                    "type": { "kind": "SCALAR", "name": "String" },
                    "isDeprecated": false
                }]
            }],
            "directives": []
        }}
    })
    .to_string()
}

/// Every request the mock endpoint received, as the `X-Token` it carried
/// (`""` when it carried none) — the server's request log.
type Log = Arc<Mutex<Vec<String>>>;

/// A mock endpoint shaped like a real multi-tenant one: the right token sees a
/// private schema, no token sees a public one, a bad token is refused. Returns
/// its URL and its request log.
fn mock_endpoint() -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let url = format!(
        "http://{}/graphql",
        listener.local_addr().expect("a bound address")
    );
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    // Daemon by convention: the harness ends the process, which ends this.
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            handle(stream, &seen);
        }
    });
    (url, log)
}

fn handle(mut stream: TcpStream, log: &Log) {
    let mut rd = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut token = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap_or(("", ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "x-token" => token = value.trim().to_string(),
            "content-length" => length = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    // Drain the body so the client sees a complete exchange, not a reset.
    let mut body = vec![0u8; length];
    let _ = rd.read_exact(&mut body);

    log.lock().expect("an unpoisoned log").push(token.clone());

    let (status, payload) = match token.as_str() {
        "secret" => ("200 OK", schema_body("secretField")),
        "" => ("200 OK", schema_body("publicField")),
        _ => (
            "401 Unauthorized",
            r#"{"errors":[{"message":"bad token"}]}"#.to_string(),
        ),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
}

struct Run {
    ok: bool,
    stdout: String,
}

/// One `gqls` invocation against the mock endpoint, with its own cache dir.
fn gqls(url: &str, cache: &Path, query: &str, header: Option<&str>) -> Run {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gqls"));
    cmd.arg(url).arg(query).arg("-q");
    if let Some(h) = header {
        cmd.arg("-H").arg(h);
    }
    let out = cmd
        .env("XDG_CACHE_HOME", cache)
        // Loopback is TTL-0 on purpose; this is the whole point of the knob.
        .env("GQLS_INTROSPECT_TTL", "3600")
        .output()
        .expect("gqls should be runnable");
    Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn temp_cache(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gqls-cache-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp cache dir");
    dir
}

fn requests(log: &Log) -> Vec<String> {
    log.lock().expect("an unpoisoned log").clone()
}

#[test]
fn a_cached_schema_is_never_served_to_different_credentials() {
    let (url, log) = mock_endpoint();
    let cache = temp_cache("creds");

    let authorized = gqls(&url, &cache, "secretField", Some("X-Token: secret"));
    assert!(
        authorized.ok && authorized.stdout.contains("secretField"),
        "the authorized run should see the private schema: {}",
        authorized.stdout
    );

    // Neither of these presents the credential that produced the cached schema,
    // so neither may be answered from it — that's a revoked token still working
    // for an hour, and one tenant handed another's schema.
    let wrong = gqls(&url, &cache, "secretField", Some("X-Token: WRONG"));
    assert!(
        !wrong.ok && !wrong.stdout.contains("secretField"),
        "a wrong token must not be served the cached schema: {}",
        wrong.stdout
    );
    let anonymous = gqls(&url, &cache, "publicField", None);
    assert!(
        anonymous.ok
            && anonymous.stdout.contains("publicField")
            && !anonymous.stdout.contains("secretField"),
        "an unauthenticated run should get the public schema: {}",
        anonymous.stdout
    );

    // The direct evidence: all three reached the network, carrying what they
    // were given. Keyed on the URL alone this log holds one entry.
    assert_eq!(
        requests(&log),
        vec!["secret", "WRONG", ""],
        "every distinct credential should have reached the server"
    );

    // ...and it is still a cache: the same credential again is a hit, so the
    // log doesn't grow.
    let again = gqls(&url, &cache, "secretField", Some("X-Token: secret"));
    assert!(again.ok, "the repeat run should succeed: {}", again.stdout);
    assert_eq!(
        requests(&log).len(),
        3,
        "the second authorized run should have been a cache hit"
    );

    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn an_endpoint_queried_without_headers_still_caches() {
    // The common path — a public endpoint, no `-H` at all — must keep its cache
    // now that headers are part of the key.
    let (url, log) = mock_endpoint();
    let cache = temp_cache("public");

    for _ in 0..2 {
        let run = gqls(&url, &cache, "publicField", None);
        assert!(
            run.ok && run.stdout.contains("publicField"),
            "the public schema should load: {}",
            run.stdout
        );
    }
    assert_eq!(
        requests(&log).len(),
        1,
        "the second run should have been a cache hit"
    );

    let _ = std::fs::remove_dir_all(&cache);
}
