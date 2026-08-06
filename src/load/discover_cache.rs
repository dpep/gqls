//! Remembers which schema a directory resolved to.
//!
//! [`super::discover`] is a full-tree walk, and it dominates a warm query: 26ms
//! of a 31ms run in a monorepo, seconds in a directory full of them. The answer
//! it computes, though, is one path that changes only when schema files are
//! added or moved — far less often than gqls is run, and an agent asking a
//! dozen questions about one schema pays the whole walk a dozen times.
//!
//! So a hit is: the file is younger than the TTL, it was written for *this*
//! directory, and the schema it names still exists. A miss walks. Misses are
//! never cached — a repo that grows its first schema shouldn't have to wait out
//! a TTL to be found — and `--refresh` bypasses the whole thing.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a remembered answer stands. An hour, matching the introspection
/// cache: long enough to cover a working session, short enough that a schema
/// added at the root is found the same afternoon. `GQLS_DISCOVER_TTL` (seconds,
/// `0` disables) overrides it.
const TTL: Duration = Duration::from_secs(3600);

/// Keep at most this many; one per directory gqls has been run in, and a miss
/// only costs a walk.
const MAX_FILES: usize = 64;

fn ttl() -> Duration {
    match std::env::var("GQLS_DISCOVER_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(secs) => Duration::from_secs(secs),
        None => TTL,
    }
}

/// The schema this directory resolved to last time, if that answer still holds.
pub fn load(dir: &Path) -> Option<PathBuf> {
    let p = path(dir)?;
    let schema = read(&p, dir, ttl())?;
    crate::detail!("discovery cache hit: {}", crate::paths::display(&p));
    Some(schema)
}

/// Remember this directory's answer (best-effort — a failed write just means
/// the next run walks again).
pub fn store(dir: &Path, schema: &Path) {
    if ttl().is_zero() {
        return;
    }
    let Some(p) = path(dir) else { return };
    if write(&p, dir, schema) {
        prune();
    }
}

/// The recorded answer in `file`, if it's fresh, was written for `dir`, and
/// still names a file that exists.
fn read(file: &Path, dir: &Path, ttl: Duration) -> Option<PathBuf> {
    if ttl.is_zero() {
        return None;
    }
    let age = file.metadata().ok()?.modified().ok()?.elapsed().ok()?;
    if age > ttl {
        return None;
    }
    let body = std::fs::read_to_string(file).ok()?;
    let (cached_dir, schema) = body.split_once('\n')?;
    // The filename is a hash; confirm against the directory it was written for
    // rather than trusting a collision, and confirm the schema is still there.
    if Path::new(cached_dir) != dir {
        return None;
    }
    let schema = PathBuf::from(schema.trim_end());
    schema.exists().then_some(schema)
}

/// Record `dir`'s answer in `file`; false if it couldn't be written.
fn write(file: &Path, dir: &Path, schema: &Path) -> bool {
    let (Some(d), Some(s)) = (dir.to_str(), schema.to_str()) else {
        return false;
    };
    let Some(parent) = file.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    std::fs::write(file, format!("{d}\n{s}\n")).is_ok()
}

/// Delete every remembered answer; returns how many were removed.
pub fn clear() -> usize {
    let Some(dir) = crate::paths::cache_dir() else {
        return 0;
    };
    let mut removed = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if is_ours(&e.path()) && std::fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

fn path(dir: &Path) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    // The walk's rules live in this crate, so an upgrade that changes what wins
    // shouldn't be answered from a file the old rules wrote.
    env!("CARGO_PKG_VERSION").hash(&mut h);
    dir.hash(&mut h);
    Some(crate::paths::cache_dir()?.join(format!("{:016x}.disc", h.finish())))
}

fn is_ours(p: &Path) -> bool {
    p.extension().is_some_and(|x| x == "disc")
}

/// Drop all but the `MAX_FILES` most recent, so a machine that roams between
/// directories doesn't accumulate one file per directory forever.
fn prune() {
    let Some(dir) = crate::paths::cache_dir() else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| is_ours(&e.path()))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect(),
        Err(_) => return,
    };
    if files.len() <= MAX_FILES {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(MAX_FILES) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, so a parallel test can't see its files.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gqls-disc-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn remembers_a_directorys_answer() {
        let d = scratch("roundtrip");
        let schema = d.join("schema.graphql");
        std::fs::write(&schema, "type Query { a: Int }").unwrap();
        let file = d.join("answer.disc");

        assert!(write(&file, &d, &schema));
        assert_eq!(read(&file, &d, TTL), Some(schema));
        // written for one directory, never served to another
        assert_eq!(read(&file, Path::new("/somewhere/else"), TTL), None);
        // and a zero TTL turns the whole thing off
        assert_eq!(read(&file, &d, Duration::ZERO), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_schema_that_moved_is_not_an_answer() {
        // The point of the check: serving a path that no longer exists would
        // turn a stale cache into a hard error on a file gqls chose itself.
        let d = scratch("moved");
        let schema = d.join("schema.graphql");
        std::fs::write(&schema, "type Query { a: Int }").unwrap();
        let file = d.join("answer.disc");
        assert!(write(&file, &d, &schema));

        std::fs::remove_file(&schema).unwrap();
        assert_eq!(read(&file, &d, TTL), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_expired_answer_is_a_miss() {
        let d = scratch("expired");
        let schema = d.join("schema.graphql");
        std::fs::write(&schema, "type Query { a: Int }").unwrap();
        let file = d.join("answer.disc");
        assert!(write(&file, &d, &schema));

        // the file was written just now, so anything shorter than that is past
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(read(&file, &d, Duration::from_millis(1)), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
