//! On-disk cache of per-record embedding vectors.
//!
//! Semantic search embeds every record, which is the slow part on a large
//! schema (thousands of ONNX inferences). The vectors depend only on the schema
//! content and the embedder, so we cache them keyed by a hash of both: repeat
//! queries on the same schema then only embed the *query*. A miss (schema
//! changed, different model) simply re-embeds and overwrites.
//!
//! Format: `MAGIC u32 | dims u32 | count u64 | count*dims f32`, all
//! little-endian — 256 bytes/record (64 f32), ~2.9 MB for GitHub's 11k records,
//! and dependency-free.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::mrl::MRL_DIMS;
use crate::model::SchemaRecord;

const MAGIC: u32 = 0x47_51_4C_31; // "GQL1"

/// Keep at most this many cache files; the oldest (by mtime) are pruned on
/// write, so switching between a handful of schemas stays cheap and stale
/// files from edited schemas don't accumulate unbounded.
const MAX_FILES: usize = 32;

/// Cache file path for this (schema, embedder, model) triple, or `None` if no
/// cache dir is resolvable.
pub fn path(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    MAGIC.hash(&mut h);
    (MRL_DIMS as u32).hash(&mut h);
    embedder_kind.hash(&mut h);
    model.unwrap_or("").hash(&mut h);
    records.len().hash(&mut h);
    // Key on exactly what gets embedded (super::record_text), so the key can
    // never drift from the embedding text — improve the text and the key moves
    // with it, no silent stale vectors.
    for r in records {
        super::record_text(r).hash(&mut h);
    }
    Some(cache_dir()?.join(format!("{:016x}.vecs", h.finish())))
}

/// Whether a cache file for this (schema, embedder, model) triple exists — a
/// cheap "is it warm?" check that reads no vectors.
pub fn exists(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> bool {
    path(records, embedder_kind, model).is_some_and(|p| p.is_file())
}

fn cache_dir() -> Option<PathBuf> {
    crate::paths::cache_dir()
}

/// Read cached vectors, or `None` if absent / stale / malformed.
pub fn load(path: &Path, count: usize) -> Option<Vec<Vec<f32>>> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    if buf.len() < 16 {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let dims = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    let n = u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize;
    // Guard the length math against a corrupt/torn `count` — checked so a bogus
    // value can't wrap and trigger a huge `with_capacity` alloc below.
    let expected = n
        .checked_mul(dims)
        .and_then(|x| x.checked_mul(4))
        .and_then(|x| x.checked_add(16));
    if magic != MAGIC || dims != MRL_DIMS || n != count || expected != Some(buf.len()) {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    let mut off = 16;
    for _ in 0..n {
        let mut v = Vec::with_capacity(dims);
        for _ in 0..dims {
            v.push(f32::from_le_bytes(buf[off..off + 4].try_into().ok()?));
            off += 4;
        }
        out.push(v);
    }
    Some(out)
}

/// Write vectors to `path` (best-effort — a cache write failure is never fatal).
pub fn store(path: &Path, vectors: &[Vec<f32>]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut buf = Vec::with_capacity(16 + vectors.len() * MRL_DIMS * 4);
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&(MRL_DIMS as u32).to_le_bytes());
    buf.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    for v in vectors {
        for &x in v {
            buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    let _ = std::fs::write(path, &buf);
}

/// Refresh a cache file's mtime on a hit, so an actively-used schema survives
/// LRU pruning even though its vectors didn't need rewriting.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Delete all but the `keep` most-recently-used cache files (by mtime).
pub fn prune(keep: usize) {
    let Some(dir) = cache_dir() else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "vecs"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect(),
        Err(_) => return,
    };
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0)); // newest first
    for (_, p) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Number of cache files to keep on a write. Public so the caller prunes with
/// the module's own policy.
pub fn max_files() -> usize {
    MAX_FILES
}

/// Delete every cache file; returns how many were removed.
pub fn clear() -> usize {
    let Some(dir) = cache_dir() else {
        return 0;
    };
    let mut removed = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "vecs")
                && std::fs::remove_file(e.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}
