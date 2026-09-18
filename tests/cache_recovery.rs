//! A damaged cache file should be survivable *and* visible.
//!
//! Recovery already worked — a corrupt record cache falls back to reparsing —
//! but said nothing, so the only symptom was a run that was mysteriously slow.
//! This drives the real binary because the logging is the behaviour under test.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCHEMA: &str = "examples/schema.graphql";

fn gqls(cache: &Path, args: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        .args(args)
        .arg(SCHEMA)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .expect("gqls should be runnable")
}

fn record_cache_files(cache: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(cache.join("gqls")) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rcds"))
        .collect()
}

#[test]
fn a_corrupt_record_cache_reparses_and_says_so() {
    let cache = std::env::temp_dir().join(format!("gqls-rcds-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).expect("a temp cache dir");

    let first = gqls(&cache, &["user"]);
    assert!(first.status.success(), "the first run should succeed");

    let files = record_cache_files(&cache);
    assert_eq!(
        files.len(),
        1,
        "one schema, one record cache file: {files:?}"
    );
    let mut bytes = std::fs::read(&files[0]).expect("the cache file should be readable");
    // Corrupt the record count rather than the magic: the magic check is the
    // easy path, and this proves the deeper decode failures are reported too.
    bytes[4] ^= 0xFF;
    std::fs::write(&files[0], &bytes).expect("writing the corrupted file");

    let second = gqls(&cache, &["-v", "user"]);
    assert!(
        second.status.success(),
        "a corrupt cache must not break the run"
    );
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "the reparsed answer should match the cached one"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("cached records unusable"),
        "the fallback should be visible at -v: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&cache);
}
