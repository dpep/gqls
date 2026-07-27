//! On-disk cache of per-record embedding vectors.
//!
//! Semantic search embeds every record, which is the slow part on a large
//! schema (thousands of ONNX inferences). The vectors depend only on the schema
//! content and the embedder, so we cache them keyed by a hash of both: repeat
//! queries on the same schema then only embed the *query*. A miss (schema
//! changed, different model) simply re-embeds and overwrites.
//!
//! Format: `MAGIC u32 | dims u32 | count u64 | count*dims f32`, all
//! little-endian. Compact (~7.5 MB for GitHub's 30k records) and dependency-free.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::mrl::MRL_DIMS;
use crate::model::SchemaRecord;

const MAGIC: u32 = 0x47_51_4C_31; // "GQL1"

/// Cache file path for this (schema, embedder, model) triple, or `None` if no
/// cache dir is resolvable.
pub fn path(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    MAGIC.hash(&mut h);
    (MRL_DIMS as u32).hash(&mut h);
    embedder_kind.hash(&mut h);
    model.unwrap_or("").hash(&mut h);
    records.len().hash(&mut h);
    for r in records {
        r.path.hash(&mut h);
        r.type_ref.hash(&mut h);
        r.description.hash(&mut h);
    }
    Some(cache_dir()?.join(format!("{:016x}.vecs", h.finish())))
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("gqls"))
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
    if magic != MAGIC || dims != MRL_DIMS || n != count || buf.len() != 16 + n * dims * 4 {
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
