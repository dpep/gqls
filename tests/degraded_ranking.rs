//! Degraded semantic ranking has to be visible to a machine, not just to a
//! human reading stderr.
//!
//! Hermetic: `GQLS_MODEL_DIR` points at an empty directory, so the ONNX load
//! fails the way it does offline and the hash fallback takes over — no network,
//! no model download. Fuzzy-only builds have no fallback to report.
#![cfg(feature = "_semantic")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCHEMA: &str = "examples/schema.graphql";

fn gqls(cache: &Path, no_model: Option<&Path>, args: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gqls"));
    cmd.arg(SCHEMA).args(args).arg("-J");
    if let Some(dir) = no_model {
        // An empty model dir: the loader finds no model.onnx and falls back.
        cmd.env("GQLS_MODEL_DIR", dir);
    }
    cmd.env("XDG_CACHE_HOME", cache)
        .env("GQLS_NO_AUTOWARM", "1")
        .output()
        .expect("gqls should be runnable")
}

fn rows(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line should be JSON"))
        .collect()
}

fn dirs(tag: &str) -> (PathBuf, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("gqls-degraded-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (cache, model) = (base.join("cache"), base.join("no-model"));
    std::fs::create_dir_all(&cache).expect("a temp cache dir");
    std::fs::create_dir_all(&model).expect("a temp model dir");
    (cache, model)
}

#[test]
fn json_says_when_the_ranking_fell_back_to_the_hash_embedder() {
    let (cache, no_model) = dirs("fallback");

    let degraded = gqls(&cache, Some(&no_model), &["--semantic", "user"]);
    let hits = rows(&degraded);
    assert!(
        !hits.is_empty(),
        "the fallback still ranks, so there should be rows to mark: {}",
        String::from_utf8_lossy(&degraded.stderr)
    );
    assert!(
        hits.iter()
            .all(|r| r["degraded"] == serde_json::json!(true)),
        "every row should carry the degraded ranking: {hits:#?}"
    );
    // The human-facing half, which is what a JSON consumer never sees.
    assert!(
        String::from_utf8_lossy(&degraded.stderr).contains("embedding model unavailable"),
        "stderr should still say it too"
    );

    // The control: fuzzy ranking doesn't use the model, so nothing is degraded
    // and the field stays absent — the flag means something only when set.
    let fuzzy = gqls(&cache, Some(&no_model), &["--fuzzy", "user"]);
    let hits = rows(&fuzzy);
    assert!(!hits.is_empty(), "the control should match something");
    assert!(
        hits.iter().all(|r| r.get("degraded").is_none()),
        "fuzzy rows should not carry the flag at all: {hits:#?}"
    );

    let _ = std::fs::remove_dir_all(cache.parent().expect("a base dir"));
}
