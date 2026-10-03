//! `--profile` names the work it times. An `output` phase that also held the
//! explain check and the miss diagnosis pointed every slow run at rendering.

mod common;

use std::process::Command;

const SCHEMA: &str = "examples/schema.graphql";

/// The phase names a `--profile -j` run reported.
fn phases(query: &str) -> Vec<String> {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args([query, SCHEMA, "-j", "-q", "--profile"])
        .output()
        .expect("gqls should be runnable");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let report = stderr
        .lines()
        .find(|l| l.contains("\"total_ms\""))
        .unwrap_or_else(|| panic!("no profile in {stderr}"));
    let report: serde_json::Value = serde_json::from_str(report).expect("profile JSON");
    report["phases"]
        .as_array()
        .expect("a phase list")
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_explain_check_is_timed_apart_from_output() {
    let names = phases("user");
    for want in ["explain check", "output"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    assert!(!names.iter().any(|n| n == "miss diagnosis"), "{names:?}");
}

#[test]
fn a_miss_times_its_diagnosis() {
    let names = phases("zqxjkw");
    assert!(names.iter().any(|n| n == "miss diagnosis"), "{names:?}");
}
