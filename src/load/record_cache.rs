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

/// Bump when the encoding or `SchemaRecord` shape changes — old files then
/// fail the magic check and re-parse.
const MAGIC: u32 = 0x47_51_52_31; // "GQR1"

/// Keep at most this many cache files (LRU by mtime, pruned on write).
const MAX_FILES: usize = 32;

/// Cached records for this source (SDL text or introspection JSON bytes), or
/// `None` on any miss (absent, stale magic, corrupt).
pub fn load(source: &[u8]) -> Option<Vec<SchemaRecord>> {
    let p = path(source)?;
    let mut buf = Vec::new();
    std::fs::File::open(&p).ok()?.read_to_end(&mut buf).ok()?;
    let records = decode(&buf)?;
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
    for r in records {
        put_str(&mut buf, &r.path);
        put_str(&mut buf, &r.name);
        put_str(&mut buf, r.kind.as_str());
        put_opt(&mut buf, r.parent.as_deref());
        put_opt(&mut buf, r.type_ref.as_deref());
        put_vec(&mut buf, &r.args);
        put_opt(&mut buf, r.description.as_deref());
        put_opt(&mut buf, r.deprecated.as_deref());
        put_vec(&mut buf, &r.directives);
    }
    buf
}

fn decode(buf: &[u8]) -> Option<Vec<SchemaRecord>> {
    let mut rd = Reader { buf, off: 0 };
    if rd.u32()? != MAGIC {
        return None;
    }
    let n = rd.u64()? as usize;
    // Guard the capacity against a corrupt count: every record costs ≥ 9 bytes.
    if n > buf.len() / 9 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(SchemaRecord {
            path: rd.str()?,
            name: rd.str()?,
            kind: rd.str()?.parse::<Kind>().ok()?,
            parent: rd.opt()?,
            type_ref: rd.opt()?,
            args: rd.vec()?,
            description: rd.opt()?,
            deprecated: rd.opt()?,
            directives: rd.vec()?,
        });
    }
    (rd.off == buf.len()).then_some(out)
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
