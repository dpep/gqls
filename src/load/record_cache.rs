//! On-disk cache of parsed schema records.
//!
//! Parsing dominates the fuzzy path on a large schema (~28ms of a ~45ms query
//! at 48k records for SDL; more for introspection JSON), and the records
//! depend only on the source bytes — SDL text or an introspection response —
//! so cache them keyed by a hash of those bytes. Same idea and same house
//! format rules as the vector cache: a small length-prefixed binary layout,
//! dependency-free. A miss (schema edited, format bumped) simply re-parses
//! and overwrites.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::PathBuf;

use crate::model::{Kind, SchemaRecord};

/// Bump when the on-disk encoding changes — old files then fail this check and
/// re-parse. Changes to what the loaders *put* in a record are covered by the
/// crate version in the key (see `path`), so they need no bump here.
const MAGIC: u32 = 0x4751_5232; // "GQR2"

/// Records per decode chunk. The file stores where each chunk starts so they
/// can be decoded in parallel — records are self-delimiting, but only from a
/// known offset, so the offsets have to be written down. Sized so a small
/// schema stays a single chunk and pays nothing for the machinery.
const CHUNK: usize = 4096;

/// Keep at most this many cache files (LRU by mtime, pruned on write).
const MAX_FILES: usize = 32;

/// Cached records for this source (SDL text or introspection JSON bytes), or
/// `None` on any miss (absent, stale magic, corrupt).
pub fn load(source: &[u8]) -> Option<Vec<SchemaRecord>> {
    let p = path(source)?;
    let mut read_span = crate::profile::span("cache: read");
    let mut buf = Vec::new();
    std::fs::File::open(&p).ok()?.read_to_end(&mut buf).ok()?;
    read_span.note(|| format!("{:.1} MB", buf.len() as f64 / 1_048_576.0));
    drop(read_span);
    let mut decode_span = crate::profile::span("cache: decode");
    let records = decode(&buf)?;
    decode_span.note(|| format!("{} records", records.len()));
    drop(decode_span);
    crate::detail!("schema cache hit: {}", crate::paths::display(&p));
    touch(&p);
    Some(records)
}

/// Write records for this source (best-effort — a cache write failure is
/// never fatal), then prune the least-recently-used files.
pub fn store(source: &[u8], records: &[SchemaRecord]) {
    let Some(p) = path(source) else {
        return;
    };
    let Some(parent) = p.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = std::fs::write(&p, encode(records));
    prune(MAX_FILES);
}

/// Delete every record cache file; returns how many were removed.
pub fn clear() -> usize {
    let Some(dir) = crate::paths::cache_dir() else {
        return 0;
    };
    let mut removed = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "rcds")
                && std::fs::remove_file(e.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

fn path(source: &[u8]) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    MAGIC.hash(&mut h);
    // Keyed by the crate version as well as the source, because the records
    // depend on how the loaders render them — teaching the SDL parser to keep
    // argument defaults changes what's cached without changing the schema a
    // byte. Re-parsing after an upgrade costs milliseconds; serving records
    // from a previous parser silently omits whatever it has since learned.
    env!("CARGO_PKG_VERSION").hash(&mut h);
    source.hash(&mut h);
    Some(crate::paths::cache_dir()?.join(format!("{:016x}.rcds", h.finish())))
}

/// Refresh mtime on a hit so an actively-used schema survives LRU pruning.
fn touch(p: &std::path::Path) {
    if let Ok(f) = std::fs::File::open(p) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Delete all but the `keep` most-recently-used record cache files.
fn prune(keep: usize) {
    let Some(dir) = crate::paths::cache_dir() else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "rcds"))
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

// --- encoding: MAGIC u32 | count u64 | records, all little-endian ---
// Strings are u32-length-prefixed UTF-8; Option is a 0/1 byte then the value;
// Vec is a u32 count then values; Kind round-trips via its stable string form.

fn encode(records: &[SchemaRecord]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64 * records.len() + 16);
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&(records.len() as u64).to_le_bytes());

    // Reserve the chunk table, then backfill it: each entry is where a run of
    // records starts, which is the one thing a decoder can't work out for
    // itself from a self-delimiting format.
    let chunks = records.len().div_ceil(CHUNK).max(1);
    buf.extend_from_slice(&(chunks as u32).to_le_bytes());
    let table_at = buf.len();
    buf.extend_from_slice(&vec![0u8; chunks * 8]);

    let mut offsets = Vec::with_capacity(chunks);
    for (i, r) in records.iter().enumerate() {
        if i % CHUNK == 0 {
            offsets.push(buf.len() as u64);
        }
        put_str(&mut buf, &r.path);
        put_str(&mut buf, &r.name);
        buf.push(kind_byte(r.kind));
        put_opt(&mut buf, r.parent.as_deref());
        put_opt(&mut buf, r.type_ref.as_deref());
        put_vec(&mut buf, &r.args);
        put_opt(&mut buf, r.description.as_deref());
        put_opt(&mut buf, r.deprecated.as_deref());
        put_vec(&mut buf, &r.directives);
        put_vec(&mut buf, &r.possible_types);
    }
    for (i, off) in offsets.iter().enumerate() {
        buf[table_at + i * 8..table_at + (i + 1) * 8].copy_from_slice(&off.to_le_bytes());
    }
    buf
}

fn decode(buf: &[u8]) -> Option<Vec<SchemaRecord>> {
    use rayon::prelude::*;

    let mut rd = Reader { buf, off: 0 };
    if rd.u32()? != MAGIC {
        return None;
    }
    let n = rd.u64()? as usize;
    // Guard the capacity against a corrupt count: every record costs ≥ 9 bytes.
    if n > buf.len() / 9 {
        return None;
    }
    let chunks = rd.u32()? as usize;
    if chunks != n.div_ceil(CHUNK).max(1) {
        return None; // table doesn't describe this record count
    }
    let offsets: Vec<u64> = (0..chunks).map(|_| rd.u64()).collect::<Option<_>>()?;

    // Decoding is where a warm query spends its time — a few hundred thousand
    // string allocations — and the chunks are independent, so spread them.
    let decoded: Vec<(Vec<SchemaRecord>, usize)> = offsets
        .par_iter()
        .enumerate()
        .map(|(i, &off)| {
            let count = CHUNK.min(n - i * CHUNK);
            decode_chunk(buf, off as usize, count)
        })
        .collect::<Option<_>>()?;

    // The last chunk has to land exactly on the end of the file — the chunked
    // read would otherwise accept a torn or padded cache that the sequential
    // one caught by consuming every byte.
    if decoded.last().map(|(_, end)| *end) != Some(buf.len()) {
        return None;
    }
    let out: Vec<SchemaRecord> = decoded.into_iter().flat_map(|(rs, _)| rs).collect();
    (out.len() == n).then_some(out)
}

/// Decode `count` records starting at `off`. Records are self-delimiting, so a
/// chunk needs nothing but its starting point.
fn decode_chunk(buf: &[u8], off: usize, count: usize) -> Option<(Vec<SchemaRecord>, usize)> {
    let mut rd = Reader { buf, off };
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_record(&mut rd)?);
    }
    Some((out, rd.off))
}

fn read_record(rd: &mut Reader) -> Option<SchemaRecord> {
    Some(SchemaRecord {
        path: rd.str()?,
        name: rd.str()?,
        kind: kind_from_byte(rd.take(1)?[0])?,
        parent: rd.opt()?,
        type_ref: rd.opt()?,
        args: rd.vec()?,
        description: rd.opt()?,
        deprecated: rd.opt()?,
        directives: rd.vec()?,
        possible_types: rd.vec()?,
    })
}

/// `Kind` as one byte. An exhaustive match, so adding a variant fails to
/// compile here rather than silently writing an unreadable cache.
fn kind_byte(k: Kind) -> u8 {
    match k {
        Kind::Object => 0,
        Kind::Interface => 1,
        Kind::Union => 2,
        Kind::Enum => 3,
        Kind::InputObject => 4,
        Kind::Scalar => 5,
        Kind::Directive => 6,
        Kind::Field => 7,
        Kind::InputField => 8,
        Kind::EnumValue => 9,
        Kind::Query => 10,
        Kind::Mutation => 11,
        Kind::Subscription => 12,
    }
}

fn kind_from_byte(b: u8) -> Option<Kind> {
    Some(match b {
        0 => Kind::Object,
        1 => Kind::Interface,
        2 => Kind::Union,
        3 => Kind::Enum,
        4 => Kind::InputObject,
        5 => Kind::Scalar,
        6 => Kind::Directive,
        7 => Kind::Field,
        8 => Kind::InputField,
        9 => Kind::EnumValue,
        10 => Kind::Query,
        11 => Kind::Mutation,
        12 => Kind::Subscription,
        _ => return None,
    })
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn put_opt(buf: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            buf.push(1);
            put_str(buf, s);
        }
        None => buf.push(0),
    }
}

fn put_vec(buf: &mut Vec<u8>, v: &[String]) {
    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
    for s in v {
        put_str(buf, s);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    off: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.off.checked_add(n)?;
        let s = self.buf.get(self.off..end)?;
        self.off = end;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(std::str::from_utf8(self.take(n)?).ok()?.to_string())
    }

    fn opt(&mut self) -> Option<Option<String>> {
        match self.take(1)?[0] {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }

    fn vec(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        // ≥ 4 bytes per element; reject a corrupt count before allocating.
        if n > self.buf.len() / 4 {
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.str()?);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str) -> SchemaRecord {
        SchemaRecord {
            path: format!("User.{name}"),
            name: name.into(),
            kind: Kind::Field,
            parent: Some("User".into()),
            type_ref: Some("String!".into()),
            args: vec!["first: Int".into(), "after: String".into()],
            description: None,
            deprecated: Some("use other".into()),
            directives: vec![],
            possible_types: vec![],
        }
    }

    #[test]
    fn round_trips_records() {
        let records = vec![rec("email"), rec("name")];
        let decoded = decode(&encode(&records)).unwrap();
        assert_eq!(decoded.len(), 2);
        let (a, b) = (&records[0], &decoded[0]);
        assert_eq!(a.path, b.path);
        assert_eq!(a.kind, b.kind);
        assert_eq!(a.parent, b.parent);
        assert_eq!(a.type_ref, b.type_ref);
        assert_eq!(a.args, b.args);
        assert_eq!(a.description, b.description);
        assert_eq!(a.deprecated, b.deprecated);
        assert_eq!(a.directives, b.directives);
    }

    #[test]
    fn rejects_corrupt_data() {
        let mut buf = encode(&[rec("email")]);
        assert!(decode(&buf[..buf.len() - 1]).is_none()); // truncated
        buf[0] ^= 0xFF; // bad magic
        assert!(decode(&buf).is_none());
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn rejects_trailing_garbage() {
        let mut buf = encode(&[rec("email")]);
        buf.push(0);
        assert!(decode(&buf).is_none());
    }
}
