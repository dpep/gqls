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
//! Each write then collects the files it superseded — a file whose keys the new
//! one wholly contains is pure duplication — so a schema that keeps gaining
//! fields keeps one file, not one per edit. What survives that is bounded by
//! both a file count and a byte budget, since one file scales with the schema.
//!
//! Format: `MAGIC u32 | dims u32 | count u64 | count * (key u128 | dims f32)`,
//! all little-endian — 272 bytes/record (64 f32 + key), dependency-free.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::mrl::MRL_DIMS;
use crate::model::SchemaRecord;

const MAGIC: u32 = 0x4751_4C33; // "GQL3" — GQL1 had no keys, GQL2 hashed unstably

/// Keep at most this many cache files; the oldest (by mtime) are pruned on
/// write, so switching between a handful of schemas stays cheap and stale
/// files from edited schemas don't accumulate unbounded.
const MAX_FILES: usize = 32;

/// How many recent same-embedder files to mine for reusable vectors on a miss.
/// One covers the common case (this schema, one edit ago); a few more cover
/// alternating between schemas without unbounded read cost.
const MAX_DONORS: usize = 4;

/// Total bytes of vectors to retain. A file count alone doesn't bound disk:
/// one 49k-record schema is ~13MB per state, so 32 of them is ~430MB.
/// Consolidation removes most of that redundancy and this backstops the rest —
/// many distinct schemas, or states kept because fields were removed.
const MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Bytes of fixed header, and per-entry key width.
const HEADER: usize = 16;
const KEY_BYTES: usize = 16;

/// Content key for one record's embedded text — 128 bits, so that a schema with
/// hundreds of thousands of records has no realistic chance of two records
/// colliding onto each other's vector.
pub type Key = (u64, u64);

// FNV-1a, vendored in ten lines rather than taken as a dependency (this module
// is otherwise dependency-free) and used in place of `DefaultHasher` because
// these hashes are an *on-disk format*, not an in-memory one. `DefaultHasher`'s
// algorithm is explicitly unspecified across Rust releases, so a toolchain bump
// could silently rename every cache file and strand every vector behind a dead
// config prefix — reintroducing the full re-embed this module exists to avoid.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a_update(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn fnv1a(salt: u64, bytes: &[u8]) -> u64 {
    fnv1a_update(FNV_OFFSET ^ salt, bytes)
}

/// Separator folded in between concatenated fields so that adjacent values
/// can't alias (`"ab" + "c"` must not hash like `"a" + "bc"`).
const SEP: &[u8] = &[0xff];

/// Hash the exact text that gets embedded, as two independently salted 64-bit
/// hashes. Not cryptographic: this only has to survive accidental collision,
/// and 128 bits over a schema's records makes that vanishingly unlikely.
pub fn key(text: &str) -> Key {
    (
        fnv1a(0xA5A5_A5A5_5A5A_5A5A, text.as_bytes()),
        fnv1a(0x1234_5678_9ABC_DEF0, text.as_bytes()),
    )
}

/// Identity of everything that invalidates a vector *other* than the record's
/// own text: format, width, embedder, model. Files sharing this prefix hold
/// vectors that are interchangeable by content key.
fn cfg_key(embedder_kind: &str, model: Option<&str>) -> u64 {
    let mut h = fnv1a(0x0C0F_0C0F_0C0F_0C0F, &MAGIC.to_le_bytes());
    h = fnv1a_update(h, &(MRL_DIMS as u32).to_le_bytes());
    h = fnv1a_update(h, embedder_kind.as_bytes());
    h = fnv1a_update(h, SEP);
    fnv1a_update(h, model.unwrap_or("").as_bytes())
}

/// Cache file path for this (schema, embedder, model) triple, or `None` if no
/// cache dir is resolvable. The name is `<config>-<schema state>` so that an
/// unchanged schema hits outright, while a changed one can still find its
/// predecessors by the shared config prefix.
pub fn path(records: &[SchemaRecord], embedder_kind: &str, model: Option<&str>) -> Option<PathBuf> {
    let mut h = fnv1a(0x5EED_5EED_5EED_5EED, &(records.len() as u64).to_le_bytes());
    // Key on exactly what gets embedded (super::record_text), so the key can
    // never drift from the embedding text — improve the text and the key moves
    // with it, no silent stale vectors.
    for r in records {
        h = fnv1a_update(h, super::record_text(r).as_bytes());
        h = fnv1a_update(h, SEP);
    }
    let name = format!("{:016x}-{h:016x}.vecs", cfg_key(embedder_kind, model));
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
    let buf = read_validated(path)?;
    let n = entry_count(&buf)?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut off = HEADER + i * (KEY_BYTES + MRL_DIMS * 4);
        let k = entry_key(&buf, i)?;
        off += KEY_BYTES;
        let mut v = Vec::with_capacity(MRL_DIMS);
        for _ in 0..MRL_DIMS {
            v.push(f32::from_le_bytes(buf[off..off + 4].try_into().ok()?));
            off += 4;
        }
        out.push((k, v));
    }
    Some(out)
}

/// Read a cache file and validate its header, or `None` if it's absent, stale,
/// or torn. Every read path funnels through here, which is what makes a
/// half-written file a clean miss rather than a wrong answer.
fn read_validated(path: &Path) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    if buf.len() < HEADER {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let dims = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
    let n = u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize;
    // Guard the length math against a corrupt/torn `count` — checked so a bogus
    // value can't wrap and trigger a huge allocation downstream.
    let expected = dims
        .checked_mul(4)
        .and_then(|x| x.checked_add(KEY_BYTES))
        .and_then(|x| x.checked_mul(n))
        .and_then(|x| x.checked_add(HEADER));
    (magic == MAGIC && dims == MRL_DIMS && expected == Some(buf.len())).then_some(buf)
}

fn entry_count(buf: &[u8]) -> Option<usize> {
    Some(u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize)
}

fn entry_key(buf: &[u8], i: usize) -> Option<Key> {
    let off = HEADER + i * (KEY_BYTES + MRL_DIMS * 4);
    let k0 = u64::from_le_bytes(buf.get(off..off + 8)?.try_into().ok()?);
    let k1 = u64::from_le_bytes(buf.get(off + 8..off + 16)?.try_into().ok()?);
    Some((k0, k1))
}

/// Just the content keys a file holds, striding past the vectors. Enough to
/// decide whether a newer file has superseded this one, without paying to
/// materialise thousands of vectors we'd only throw away.
fn keys_in(path: &Path) -> Option<Vec<Key>> {
    let buf = read_validated(path)?;
    (0..entry_count(&buf)?)
        .map(|i| entry_key(&buf, i))
        .collect()
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
/// `refresh` is the user's "distrust the cache" flag: it must suppress *reuse*,
/// not merely the whole-file hit. Mining donors under `--refresh` would hand
/// back the very vectors it was asked to rebuild — the flag exists for changes
/// the content key cannot see, like model weights replaced under the same name.
pub fn reusable(embedder_kind: &str, model: Option<&str>, refresh: bool) -> HashMap<Key, Vec<f32>> {
    let Some(dir) = cache_dir() else {
        return HashMap::new();
    };
    reusable_in(
        &dir,
        &format!("{:016x}-", cfg_key(embedder_kind, model)),
        refresh,
    )
}

/// [`reusable`] against an explicit directory and file prefix — the seam the
/// tests drive, so they never touch the real cache dir.
fn reusable_in(dir: &Path, prefix: &str, refresh: bool) -> HashMap<Key, Vec<f32>> {
    let mut map = HashMap::new();
    if refresh {
        return map;
    }
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
    // Write-then-rename, because `gqls --warm` runs detached and can be writing
    // while a foreground query reads. A reader already treats a torn file as a
    // miss (the length check in `read_entries`), so this isn't about wrong
    // answers — it's that a half-written file would send the foreground into a
    // pointless re-embed. Rename is atomic within a filesystem, so readers only
    // ever observe a complete file. The pid keeps two writers off one temp.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, &buf).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Delete the files the freshly written one has superseded: any file for the
/// same config whose keys it wholly contains. Returns how many were collapsed.
///
/// Files are otherwise near-duplicates — a schema that gains a field writes a
/// complete new copy and leaves the old one behind, so a lineage accumulates
/// one full file per edit. In that ordinary case the previous state is a strict
/// subset and collapses away here, leaving one file per lineage instead of one
/// per edit. Reverting to the older schema stays instant even so: every vector
/// it needs is present in the file that replaced it, so mining finds them all
/// and embeds nothing. A file holding keys the new one lacks — fields were
/// removed — is not superseded, and is kept.
pub fn consolidate(
    written: &Path,
    keys: &[Key],
    embedder_kind: &str,
    model: Option<&str>,
) -> usize {
    let Some(dir) = cache_dir() else {
        return 0;
    };
    consolidate_in(
        &dir,
        written,
        keys,
        &format!("{:016x}-", cfg_key(embedder_kind, model)),
    )
}

/// [`consolidate`] against an explicit directory and prefix — the test seam.
fn consolidate_in(dir: &Path, written: &Path, keys: &[Key], prefix: &str) -> usize {
    let live: std::collections::HashSet<Key> = keys.iter().copied().collect();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut collapsed = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p == written
            || p.extension().is_none_or(|x| x != "vecs")
            || !e.file_name().to_string_lossy().starts_with(prefix)
        {
            continue;
        }
        // Unreadable/stale files are left to the LRU rather than deleted here —
        // this function's remit is redundancy, not corruption.
        let Some(theirs) = keys_in(&p) else {
            continue;
        };
        if theirs.iter().all(|k| live.contains(k)) && std::fs::remove_file(&p).is_ok() {
            collapsed += 1;
        }
    }
    collapsed
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
    // Sweep temp files a crashed writer left behind — they carry a `.tmp<pid>`
    // extension, so they're invisible to every other path in this module and
    // would otherwise accumulate silently.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let stale = e
                .path()
                .extension()
                .is_some_and(|x| x.to_string_lossy().starts_with("tmp"));
            if stale {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().is_ok_and(|d| d.as_secs() > 3600));
                if old.unwrap_or(false) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    prune_in(&dir, keep, MAX_BYTES);
}

/// [`prune`] against an explicit directory and budget — the test seam. Evicts
/// by recency on two independent limits: a file count, and a total byte budget
/// (a count can't bound disk when one file scales with schema size). The newest
/// file always survives, even if it alone exceeds the budget — evicting the
/// cache you just wrote would guarantee a re-embed on the very next run.
fn prune_in(dir: &Path, keep: usize, max_bytes: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "vecs"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect(),
        Err(_) => return,
    };
    files.sort_by_key(|f| std::cmp::Reverse(f.0)); // newest first
    let mut total = 0u64;
    for (i, (_, size, p)) in files.iter().enumerate() {
        total = total.saturating_add(*size);
        if i > 0 && (i >= keep || total > max_bytes) {
            let _ = std::fs::remove_file(p);
        }
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

        let map = reusable_in(&dir, "aaaa-", false);
        assert!(map.contains_key(&key("kept")), "reuses the previous state");
        assert!(map.contains_key(&key("added")), "mines every recent file");
        assert!(
            !map.contains_key(&key("other")),
            "a different embedder's vectors must never be reused"
        );

        // --refresh must suppress reuse, not just the whole-file hit: otherwise
        // it hands back the very vectors the user asked to rebuild.
        assert!(
            reusable_in(&dir, "aaaa-", true).is_empty(),
            "refresh must reuse nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consolidation_collects_superseded_files_only() {
        let dir = std::env::temp_dir().join("gqls-cache-test-consolidate");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let old = dir.join("aaaa-0001.vecs"); // the previous state: a subset
        let removed = dir.join("aaaa-0002.vecs"); // holds a key the new one lacks
        let foreign = dir.join("bbbb-0003.vecs"); // another embedder entirely
        let new = dir.join("aaaa-0004.vecs");
        store(&old, &[key("a"), key("b")], &vecs(2));
        store(&removed, &[key("a"), key("gone")], &vecs(2));
        store(&foreign, &[key("a")], &vecs(1));
        let live = [key("a"), key("b"), key("c")];
        store(&new, &live, &vecs(3));

        assert_eq!(consolidate_in(&dir, &new, &live, "aaaa-"), 1);
        assert!(!old.exists(), "a superseded state must be collected");
        assert!(
            removed.exists(),
            "a file holding keys the new one lacks is not superseded"
        );
        assert!(foreign.exists(), "another embedder's file is untouchable");
        assert!(new.exists(), "never collect the file just written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_honours_both_the_count_and_the_byte_budget() {
        let dir = std::env::temp_dir().join("gqls-cache-test-prune");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        // four files, newest last written; each is one entry (~272 bytes)
        let paths: Vec<PathBuf> = (0..4).map(|i| dir.join(format!("p-{i}.vecs"))).collect();
        for (i, p) in paths.iter().enumerate() {
            store(p, &[key(&i.to_string())], &vecs(1));
            // distinct, increasing mtimes so "newest" is unambiguous
            let t =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 + i as u64);
            let _ = std::fs::File::open(p).map(|f| f.set_modified(t));
        }
        // byte budget admits about two files; the count limit is not the binding one
        prune_in(&dir, 99, 600);
        assert!(paths[3].exists(), "newest survives");
        assert!(paths[2].exists(), "second-newest fits the budget");
        assert!(!paths[0].exists() && !paths[1].exists(), "oldest evicted");

        // a budget smaller than a single file must still leave the newest
        prune_in(&dir, 99, 1);
        assert!(paths[3].exists(), "never evict the only current cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_keys_are_stable_across_builds() {
        // These are an on-disk format, not just an in-memory hash — if this
        // assertion ever fails, every user's cache silently invalidates and the
        // full re-embed this module exists to prevent comes back. Change the
        // hash only with a MAGIC bump.
        // Published FNV-1a 64 vectors — these check the algorithm is actually
        // FNV, not merely that it agrees with yesterday's build.
        assert_eq!(fnv1a(0, b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(0, b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(0, b"foobar"), 0x8594_4171_f739_67e8);
        // A frozen key, so a change to the salts or the composition can't slip
        // through unnoticed.
        assert_eq!(
            key("Query.user"),
            (0xc093_f72a_7006_68de, 0xc39c_d88a_c01d_7f54)
        );
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
