//! End-to-end checks on the batch form: queries piped on stdin, one per line,
//! answered by a single process. Drives the real binary, because the whole
//! feature is about argument routing and stream shape — the parts a library
//! test can't see. Fuzzy queries only, so this holds on every feature build.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};

const SCHEMA: &str = "examples/schema.graphql";

/// Run the binary with `stdin` piped in, returning stdout.
fn run(args: &[&str], stdin: &str) -> String {
    // Cargo will occasionally hand back a binary older than the source and
    // swear it's current; without this the failures look like real bugs.
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        // --fuzzy keeps this off the embedding model: batch routing is what's
        // under test, and a semantic build shouldn't make the test slower or
        // dependent on a downloaded model.
        .arg("--fuzzy")
        .arg("-q")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("gqls should be runnable");
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(stdin.as_bytes())
        .expect("writing queries to stdin");
    let out = child.wait_with_output().expect("gqls should exit");
    String::from_utf8(out.stdout).expect("stdout should be utf-8")
}

fn rows(out: &str) -> Vec<serde_json::Value> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line should be JSON"))
        .collect()
}

#[test]
fn every_row_says_which_query_produced_it() {
    // Without this a consumer can't tell whose rows are whose once several
    // queries share one stream.
    let out = run(&[SCHEMA, "-J", "-l", "2"], "user\ncreateUser\n");
    let rows = rows(&out);
    assert!(rows.len() >= 2, "expected hits for both queries: {out}");

    let queries: Vec<&str> = rows.iter().filter_map(|r| r["query"].as_str()).collect();
    assert!(queries.contains(&"user"), "missing rows for `user`: {out}");
    assert!(
        queries.contains(&"createUser"),
        "missing rows for `createUser`: {out}"
    );
    assert_eq!(queries.len(), rows.len(), "every row carries its query");
}

#[test]
fn a_query_that_matches_nothing_still_appears() {
    // A miss that emitted nothing would be indistinguishable from a query that
    // was never run, which is the one thing a batch consumer can't recover.
    let out = run(&[SCHEMA, "-J"], "user\nNoSuchType.*\n");
    let miss = rows(&out)
        .into_iter()
        .find(|r| r["query"] == "NoSuchType.*")
        .expect("the unmatched query should still be reported");
    assert_eq!(miss["status"], "no_matches");
}

#[test]
fn blank_lines_are_not_queries() {
    let out = run(&[SCHEMA, "-J", "-l", "1"], "\n\nuser\n\n");
    let queries: Vec<String> = rows(&out)
        .iter()
        .filter_map(|r| r["query"].as_str().map(str::to_string))
        .collect();
    assert_eq!(queries, ["user"], "only the real query runs: {out}");
}

#[test]
fn a_single_query_keeps_the_shape_it_always_had() {
    // The `query` field is batch-only: adding it unconditionally would change
    // the output every existing caller parses.
    let out = run(&["user", SCHEMA, "-J", "-l", "1"], "");
    let row = &rows(&out)[0];
    assert!(row.get("query").is_none(), "single query stays bare: {out}");
    assert_eq!(row["name"], "user");
}

#[test]
fn an_explicit_query_wins_over_piped_input() {
    // A positional that isn't a schema path is a query, pipe or no pipe —
    // otherwise a stray redirect would silently replace what was asked for.
    let out = run(&["user", SCHEMA, "-J", "-l", "1"], "createUser\n");
    let rows = rows(&out);
    assert_eq!(rows.len(), 1, "only the explicit query ran: {out}");
    assert_eq!(rows[0]["name"], "user");
}

/// A batch answers each query as it arrives, rather than draining stdin first.
///
/// The assertion is the *timing*, because the output is byte-identical either
/// way: draining first makes `producer | gqls -J` silent until the producer
/// closes, which for a long-running producer means silent forever.
#[test]
fn a_batch_answers_before_the_producer_closes() {
    use std::io::{BufRead, BufReader};

    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args([SCHEMA, "-J", "--fuzzy", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("gqls should be runnable");

    let mut stdin = child.stdin.take().expect("stdin was piped");
    writeln!(stdin, "user").expect("writing the first query");
    stdin.flush().expect("flushing the first query");

    // Read with stdin still open. If the batch drained first this blocks
    // forever, so a reader thread bounds the wait.
    let mut out = BufReader::new(child.stdout.take().expect("stdout was piped"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = out.read_line(&mut line);
        let _ = tx.send(line);
    });

    let first = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("no answer before EOF: the batch is draining stdin, not streaming");
    assert!(first.contains("\"query\""), "{first}");

    drop(stdin);
    child.wait().expect("gqls should exit");
}
