//! What `gqls` says about the schema it picked when no source was passed.
//!
//! Drives the real binary in a scratch directory: discovery reads the process's
//! current directory, which a library test can't set without racing every other
//! test in the binary.

mod common;

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gqls-discovery-test-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `gqls <query> -v --refresh` in `dir`, returning stderr.
fn discover_in(dir: &PathBuf) -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        // --refresh so the answer is walked, not remembered from a previous run
        .args(["a", "-v", "--refresh"])
        .current_dir(dir)
        .output()
        .expect("gqls should be runnable");
    String::from_utf8(out.stderr).expect("stderr should be utf-8")
}

#[test]
fn a_tie_between_two_schemas_is_reported() {
    // Same tier, same directory, same depth — settled alphabetically, and the
    // one thing a user in this position needs to know is that it was settled.
    let root = scratch("tie");
    std::fs::write(root.join("api.graphqls"), "type Query { a: Int }").unwrap();
    std::fs::write(root.join("legacy.graphqls"), "type Query { a: Int }").unwrap();

    let err = discover_in(&root);
    assert!(err.contains("using schema api.graphqls"), "{err}");
    assert!(err.contains("1 other schema file(s) found"), "{err}");

    // and with nothing to be ambiguous about, there's nothing to report
    std::fs::remove_file(root.join("legacy.graphqls")).unwrap();
    let err = discover_in(&root);
    assert!(err.contains("using schema api.graphqls"), "{err}");
    assert!(!err.contains("other schema file"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}
