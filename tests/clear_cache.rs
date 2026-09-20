//! `--clear-cache` deletes gqls's own cache files — every kind it writes or
//! used to — and nothing else. It shipped as "every file under the cache dir",
//! and an empty `XDG_CACHE_HOME` put that dir in the cwd: a project directory
//! named `gqls` lost its source.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gqls-clear-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    dir
}

fn write(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
    std::fs::write(path, b"x").expect("a file");
}

fn clear(env: &[(&str, &Path)], cwd: &Path) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gqls"));
    cmd.arg("--clear-cache").current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("gqls should be runnable")
}

#[test]
fn clears_every_kind_of_file_gqls_writes_including_old_ones() {
    let root = scratch("kinds");
    let dir = root.join("gqls");
    for f in [
        "introspect/0123.json",
        "0123.rcds",
        "0123.disc",
        "0123-4567.vecs",
        "0123.tmp42",
    ] {
        write(&dir.join(f));
    }
    let out = clear(&[("XDG_CACHE_HOME", &root)], &root);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let left: Vec<_> = walk(&dir);
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success(), "{stderr}");
    assert!(left.is_empty(), "left behind: {left:?}");
    assert!(stderr.contains("cleared 5 cached file(s)"), "{stderr}");
}

#[test]
fn leaves_files_it_did_not_write_and_never_follows_a_symlink() {
    let root = scratch("foreign");
    let dir = root.join("gqls");
    write(&dir.join("notes.txt"));
    write(&dir.join("stray.json"));
    write(&root.join("victim/a.rcds"));
    write(&root.join("elsewhere/b.json"));
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("victim"), dir.join("link")).expect("a symlink");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("elsewhere"), dir.join("introspect")).expect("a symlink");

    let out = clear(&[("XDG_CACHE_HOME", &root)], &root);
    let notes = dir.join("notes.txt").exists() && dir.join("stray.json").exists();
    let victim = root.join("victim/a.rcds").exists() && root.join("elsewhere/b.json").exists();
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success());
    assert!(notes, "deleted a file gqls didn't write");
    assert!(victim, "followed a symlink out of the cache dir");
}

#[test]
fn an_empty_or_relative_cache_home_never_points_at_the_cwd() {
    let root = scratch("cwd");
    let project = root.join("proj");
    write(&project.join("gqls/src/main.rs"));
    write(&project.join("gqls/keep.rcds"));
    write(&project.join("rel/gqls/keep.rcds"));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    for xdg in ["", "rel"] {
        let out = clear(
            &[("XDG_CACHE_HOME", Path::new(xdg)), ("HOME", &home)],
            &project,
        );
        assert!(out.status.success(), "{xdg:?}");
    }
    let left = walk(&project);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(left.len(), 3, "the cwd lost files: {left:?}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        match p.is_dir() {
            true => out.extend(walk(&p)),
            false => out.push(p),
        }
    }
    out
}

#[test]
fn only_gqls_shaped_temp_files_are_cleared() {
    // gqls writes `<name>.tmp<pid>`. "anything starting with tmp" also took
    // `.tmpl` templates with it.
    let root = scratch("tmpl");
    let dir = root.join("gqls");
    for f in ["index.tmpl", "data.tmpx", "0123.tmp42"] {
        write(&dir.join(f));
    }
    let out = clear(&[("XDG_CACHE_HOME", &root)], &root);
    let left: Vec<_> = walk(&dir);
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success());
    assert_eq!(left.len(), 2, "took files it didn't write: {left:?}");
}

#[test]
fn a_cache_dir_that_is_a_symlink_is_cleared_like_any_other() {
    // Writes follow it, so clearing must too — it said "cleared 0" over a full
    // cache, which reads exactly like "there was nothing there".
    let root = scratch("symlinked");
    let real = root.join("real");
    write(&real.join("0123.rcds"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, root.join("gqls")).expect("a symlink");

    let out = clear(&[("XDG_CACHE_HOME", &root)], &root);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let left = walk(&real);
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success(), "{stderr}");
    assert!(left.is_empty(), "left behind: {left:?}");
    assert!(stderr.contains("cleared 1 cached file(s)"), "{stderr}");
}

#[test]
fn a_cache_home_that_cannot_be_used_is_said_aloud() {
    // Ignoring a relative or empty XDG_CACHE_HOME is the spec's rule and the
    // safe one — but the user named a directory and gqls used another.
    let root = scratch("relative");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("a home");
    let out = clear(
        &[("XDG_CACHE_HOME", Path::new("relcache")), ("HOME", &home)],
        &root,
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("XDG_CACHE_HOME"), "{stderr}");
    assert!(
        stderr.contains("relcache"),
        "should name what it ignored: {stderr}"
    );
}

#[test]
fn having_nowhere_to_cache_is_not_reported_as_an_empty_cache() {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg("--clear-cache")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .output()
        .expect("gqls should be runnable");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("cleared 0"),
        "reads as an empty cache: {stderr}"
    );
    assert!(stderr.contains("no cache directory"), "{stderr}");
}
