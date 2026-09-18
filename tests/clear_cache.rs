//! `--clear-cache` empties gqls's cache directory, whatever is in it — files
//! an older release wrote (embedding vectors, an interrupted write's temp
//! file) included, since "every cached file" is what it promises.

mod common;

use std::path::Path;
use std::process::Command;

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p.display().to_string());
        }
    }
    out
}

#[test]
fn clear_cache_removes_every_file_including_ones_it_no_longer_writes() {
    let cache = std::env::temp_dir().join(format!("gqls-clear-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let dir = cache.join("gqls");
    std::fs::create_dir_all(dir.join("introspect")).expect("a temp cache dir");
    for f in [
        "introspect/0123.json",
        "0123.rcds",
        "0123.disc",
        "0123-4567.vecs",
        "0123.rcds.tmp42",
    ] {
        std::fs::write(dir.join(f), b"x").expect("a cache file");
    }

    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg("--clear-cache")
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("gqls should be runnable");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let left = files_under(&dir);
    let _ = std::fs::remove_dir_all(&cache);

    assert!(out.status.success(), "{stderr}");
    assert!(left.is_empty(), "left behind: {left:?}");
    assert!(stderr.contains("cleared 5 cached file(s)"), "{stderr}");
}
