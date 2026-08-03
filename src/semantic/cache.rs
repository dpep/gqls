//! On-disk cache of per-record embedding vectors.
//!
//! Semantic search embeds every record, which is the slow part on a large
//! schema (tens of thousands of ONNX inferences). A vector depends only on the
//! text embedded for that one record, so each is stored next to a hash of that
//! text and the whole file is named for the schema state it represents.
//!
//! That naming gives an unchanged schema an instant whole-file hit. When the
//! schema *has* changed, the new name misses — but the previous file's vectors
//! are still valid for every record that didn't change, so we reload recent
//! files for the same embedder and reuse by content hash, embedding only what's
//! actually new. Routine drift (a few fields a week) costs a few inferences
//! rather than a full re-embed.
//!
//! Format: `MAGIC u32 | dims u32 | count u64 | count * (key u128 | dims f32)`,
//! all little-endian — 272 bytes/record (64 f32 + key), dependency-free.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::mrl::MRL_DIMS;
use crate::model::SchemaRecord;

const MAGIC: u32 = 0x4751_4C32; // "GQL2" — GQL1 lacked per-record keys

/// Keep at most this many cache files; the oldest (by mtime) are pruned on
/// write, so switching between a handful of schemas stays cheap and stale
/// files from edited schemas don't accumulate unbounded.
const MAX_FILES: usize = 32;

/// How many recent same-embedder files to mine for reusable vectors on a miss.
/// One covers the common case (this schema, one edit ago); a few more cover
/// alternating between schemas without unbounded read cost.
const MAX_DONORS: usize = 4;

/// Bytes of fixed header, and per-entry key width.
const HEADER: usize = 16;
const KEY_BYTES: usize = 16;

/// Content key for one record's embedded text — 128 bits, so that a schema with
/// hundreds of thousands of records has no realistic chance of two records
/// colliding onto each other's vector.
pub type Key = (u64, u64);

/// Hash the exact text that gets embedded. Two independently salted 64-bit
/// hashes; `DefaultHasher` is not cryptographic, but this only has to survive
/// accidental collision, not an adversary.
pub fn key(text: &str) -> Key {
    let hash_with = |salt: u64| {
        let mut h = DefaultHasher::new();
        salt.hash(&mut h);
        text.hash(&mut h);
        h.finish()
    };
    (
        hash_with(0xA5A5_A5A5_5A5A_5A5A),
        hash_with(0x1234_5678_9ABC_DEF0),
    )
}

/// Identity of everything that invalidates a vector *other* than the record's
/// own text: format, width, embedder, model. Files sharing this prefix hold
/// vectors that are interchangeable by content key.
fn cfg_key(embedder_kind: &str, model: Option<&str>) -> u64 {
    let mut h = DefaultHasher::new();
    MAGIC.hash(&mut h);
    (MRL_DIMS as u32).hash(&mut h);
    embedder_kind.hash(&mut h);
    model.unwrap_or("").hash(&mut h);
    h.finish()
}

/// Cache file path for this (schema, embedder, model) triple, or `None` if no
/// cache dir is resolvable. The name is `<config>-<schema state>` so that an
/// unchanged schema hits outright, while a changed one can still find its
/// predecessors by the shared config prefix.
pub fn path(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    records.len().hash(&mut h);
    // Key on exactly what gets embedded (super::record_text), so the key can
    // never drift from the embedding text — improve the text and the key moves
    // with it, no silent stale vectors.
    for r in records {
        super::record_text(r).hash(&mut h);
    }
    let name = format!(
        "{:016x}-{:016x}.vecs",
        cfg_key(embedder_kind, model),
        h.finish()
    );
    Some(cache_dir()?.join(name))
}

/// Whether a cache file for this (schema, embedder, model) triple exists — a
/// cheap "is it warm?" check that reads no vectors.
pub fn exists(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> bool {
    path(records, embedder_kind, model).is_some_and(|p| p.is_file())
}

fn cache_dir() -> Option<PathBuf> {
    crate::paths::cache_dir()
}

/// Parse a cache file into its `(key, vector)` entries, or `None` if absent /
/// stale / malformed.
fn read_entries(path: &Path) -> Option<Vec<(Key, Vec<f32>)>> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    if buf.len() < HEADER {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let dims = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    let n = u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize;
    // Guard the length math against a corrupt/torn `count` — checked so a bogus
    // value can't wrap and trigger a huge `with_capacity` alloc below.
    let expected = dims
        .checked_mul(4)
        .and_then(|x| x.checked_add(KEY_BYTES))
        .and_then(|x| x.checked_mul(n))
        .and_then(|x| x.checked_add(HEADER));
    if magic != MAGIC || dims != MRL_DIMS || expected != Some(buf.len()) {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    let mut off = HEADER;
    for _ in 0..n {
        let k0 = u64::from_le_bytes(buf[off..off + 8].try_into().ok()?);
        let k1 = u64::from_le_bytes(buf[off + 8..off + 16].try_into().ok()?);
        off += KEY_BYTES;
        let mut v = Vec::with_capacity(dims);
        for _ in 0..dims {
            v.push(f32::from_le_bytes(buf[off..off + 4].try_into().ok()?));
            off += 4;
        }
        out.push(((k0, k1), v));
    }
    Some(out)
}

/// Read cached vectors for an exact whole-schema hit, in file order (which is
/// record order). `None` if the file is absent, stale, or doesn't hold exactly
/// `count` vectors.
pub fn load(path: &Path, count: usize) -> Option<Vec<Vec<f32>>> {
    let entries = read_entries(path)?;
    (entries.len() == count).then(|| entries.into_iter().map(|(_, v)| v).collect())
}

/// Every vector we can still use after a schema change, keyed by content. Reads
/// the most recent files for this embedder config — their vectors are valid for
/// any record whose embedded text is unchanged, whichever schema state wrote
/// them.
pub fn reusable(embedder_kind: &str, model: Option<&str>) -> HashMap<Key, Vec<f32>> {
    let Some(dir) = cache_dir() else {
        return HashMap::new();
    };
    reusable_in(&dir, &format!("{:016x}-", cfg_key(embedder_kind, model)))
}

/// [`reusable`] against an explicit directory and file prefix — the seam the
/// tests drive, so they never touch the real cache dir.
fn reusable_in(dir: &Path, prefix: &str) -> HashMap<Key, Vec<f32>> {
    let mut map = HashMap::new();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| {
                e.path().extension().is_some_and(|x| x == "vecs")
                    && e.file_name().to_string_lossy().starts_with(prefix)
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect(),
        Err(_) => return map,
    };
    files.sort_by_key(|f| std::cmp::Reverse(f.0)); // newest first
    for (_, p) in files.into_iter().take(MAX_DONORS) {
        for (k, v) in read_entries(&p).unwrap_or_default() {
            map.entry(k).or_insert(v);
        }
    }
    map
}

/// Write keyed vectors to `path` (best-effort — a cache write failure is never
/// fatal). `keys` and `vectors` are parallel and record-ordered.
pub fn store(path: &Path, keys: &[Key], vectors: &[Vec<f32>]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if keys.len() != vectors.len() || std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut buf = Vec::with_capacity(HEADER + vectors.len() * (KEY_BYTES + MRL_DIMS * 4));
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&(MRL_DIMS as u32).to_le_bytes());
    buf.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    for (&(k0, k1), v) in keys.iter().zip(vectors) {
        buf.extend_from_slice(&k0.to_le_bytes());
        buf.extend_from_slice(&k1.to_le_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vecs(n: usize) -> Vec<Vec<f32>> {
        (0..n).map(|i| vec![i as f32; MRL_DIMS]).collect()
    }

    #[test]
    fn round_trips_keys_and_vectors() {
        let dir = std::env::temp_dir().join("gqls-cache-test-roundtrip");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("a.vecs");
        let keys = vec![key("one"), key("two")];
        store(&p, &keys, &vecs(2));

        assert_eq!(load(&p, 2), Some(vecs(2)));
        // a count that doesn't match the file is a miss, not a wrong answer
        assert_eq!(load(&p, 3), None);
        let entries = read_entries(&p).unwrap();
        assert_eq!(entries[0].0, key("one"));
        assert_eq!(entries[1].0, key("two"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuse_mines_matching_files_and_ignores_foreign_ones() {
        let dir = std::env::temp_dir().join("gqls-cache-test-reuse");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // two states of the same schema, same embedder config
        store(&dir.join("aaaa-0001.vecs"), &[key("kept")], &vecs(1));
        store(&dir.join("aaaa-0002.vecs"), &[key("added")], &vecs(1));
        // a different embedder config — its vectors are NOT interchangeable
        store(&dir.join("bbbb-0001.vecs"), &[key("other")], &vecs(1));

        let map = reusable_in(&dir, "aaaa-");
        assert!(map.contains_key(&key("kept")), "reuses the previous state");
        assert!(map.contains_key(&key("added")), "mines every recent file");
        assert!(
            !map.contains_key(&key("other")),
            "a different embedder's vectors must never be reused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_tracks_the_text_exactly() {
        assert_eq!(key("User.email"), key("User.email"));
        assert_ne!(key("User.email"), key("User.emails"));
    }

    #[test]
    fn a_changed_schema_gets_a_new_name_but_the_same_config_prefix() {
        use crate::model::Kind;
        let rec = |name: &str| SchemaRecord {
            path: format!("User.{name}"),
            name: name.into(),
            kind: Kind::Field,
            parent: Some("User".into()),
            type_ref: None,
            description: None,
            deprecated: None,
            directives: vec![],
            possible_types: vec![],
            args: vec![],
        };
        let before = vec![rec("email")];
        let after = vec![rec("email"), rec("phone")];
        let (Some(a), Some(b)) = (path(&before, "onnx", None), path(&after, "onnx", None)) else {
            return; // no cache dir in this environment
        };
        assert_ne!(a, b, "a schema edit must not reuse the file wholesale");
        let prefix = |p: &PathBuf| {
            p.file_name().unwrap().to_string_lossy()[..17].to_string() // "<cfg>-"
        };
        assert_eq!(
            prefix(&a),
            prefix(&b),
            "same embedder config must share a prefix so vectors can be mined"
        );
        // a different embedder must not share the prefix
        let other = path(&before, "hash", None).unwrap();
        assert_ne!(prefix(&a), prefix(&other));
    }
}
