//! Schema ingestion: a source string is either an http(s) URL (introspect)
//! or a path to an SDL file. Both produce the same [`SchemaRecord`] list, so
//! nothing downstream cares which it was. When no source is given,
//! [`discover`] finds a schema in the current directory tree.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::model::SchemaRecord;

pub mod introspect;
pub mod record_cache;
pub mod sdl;

/// Options that shape loading. Only URL introspection and SDL files consult
/// them; introspection JSON files ignore them.
#[derive(Default)]
pub struct LoadOptions {
    /// Extra request headers for URL introspection, as `(name, value)` — e.g. an
    /// `Authorization` token for an auth-gated endpoint.
    pub headers: Vec<(String, String)>,
    /// Bypass the introspection response and parsed-record caches.
    pub refresh: bool,
}

/// Load a schema from a file path or an http(s) URL and flatten it to records.
pub fn load(source: &str, opts: &LoadOptions) -> Result<Vec<SchemaRecord>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        introspect::from_url(source, opts)
    } else if source.ends_with(".json") {
        introspect::from_json_file(source, opts)
    } else {
        let text = std::fs::read_to_string(source).map_err(|e| anyhow!("reading {source}: {e}"))?;
        if !opts.refresh {
            if let Some(records) = record_cache::load(text.as_bytes()) {
                return Ok(records);
            }
        }
        let records = sdl::from_sdl(&text)?;
        record_cache::store(text.as_bytes(), &records);
        Ok(records)
    }
}

const NOISE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "tmp",
    "dist",
    "build",
    ".venv",
];
const MAX_DEPTH: usize = 12;

/// Find a schema when none was passed: walk the current directory tree for
/// schema *documents* (not operation docs), preferring `.graphqls`, then a
/// `schema.*` file, then any `.graphql`/`.gql` whose contents look like SDL.
/// Nearest (shallowest) wins; other candidates elsewhere are reported.
pub fn discover() -> Result<String> {
    let root = std::env::current_dir()?;
    let mut found: Vec<Candidate> = Vec::new();
    walk(&root, 0, &mut found);

    if found.is_empty() {
        bail!(
            "no GraphQL schema found under {} — pass a .graphql file or an http(s) URL",
            root.display()
        );
    }

    found.sort_by(|a, b| {
        // a `supergraph*` schema wins outright (the composed graph); otherwise
        // nearest-and-most-canonical by tier, then depth, then path.
        b.supergraph
            .cmp(&a.supergraph)
            .then(a.tier.cmp(&b.tier))
            .then(a.depth.cmp(&b.depth))
            .then(a.path.cmp(&b.path))
    });
    let chosen = found.remove(0);

    let elsewhere = found
        .iter()
        .filter(|c| c.path.parent() != chosen.path.parent())
        .count();
    crate::status!("using schema {}", rel(&root, &chosen.path));
    if elsewhere > 0 {
        crate::status!(
            "{elsewhere} other schema file(s) found elsewhere — pass a path to pick one"
        );
    }

    Ok(chosen.path.to_string_lossy().into_owned())
}

struct Candidate {
    path: PathBuf,
    depth: usize,
    /// Lower = more likely to be *the* schema.
    tier: u8,
    /// A `supergraph*`-named schema — the composed graph in a federated
    /// monorepo, preferred when several candidates exist.
    supergraph: bool,
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Candidate>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || NOISE_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk(&path, depth + 1, out);
        } else if ft.is_file() {
            if let Some(tier) = classify(&path) {
                let supergraph = path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("supergraph"));
                out.push(Candidate {
                    path,
                    depth,
                    tier,
                    supergraph,
                });
            }
        }
    }
}

/// The schema-source tier of a file (lower = better), or `None` if it isn't
/// a schema document.
fn classify(path: &Path) -> Option<u8> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".graphqls") {
        return Some(0);
    }
    let sdl_ext = name.ends_with(".graphql") || name.ends_with(".gql");
    if sdl_ext && name.starts_with("schema.") {
        return Some(1);
    }
    if name.ends_with(".json") && sniff_is_introspection(path) {
        return Some(2);
    }
    if sdl_ext && sniff_is_schema(path) {
        return Some(3);
    }
    None
}

/// Cheap check that a `.json` file is a GraphQL introspection dump — the marker
/// sits in the first few KB, so we don't read a multi-MB file to reject it.
fn sniff_is_introspection(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4096];
    let n = std::io::BufReader::new(f).read(&mut head).unwrap_or(0);
    let s = String::from_utf8_lossy(&head[..n]);
    s.contains("__schema") || s.contains("\"queryType\"")
}

/// Cheap check that a `.graphql` file is SDL (type definitions) rather than an
/// operation document (`query`/`mutation` blocks) — avoids a full parse of the
/// many query files a project may carry.
fn sniff_is_schema(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("type ")
            || t.starts_with("interface ")
            || t.starts_with("input ")
            || t.starts_with("enum ")
            || t.starts_with("scalar ")
            || t.starts_with("union ")
            || t.starts_with("directive ")
            || t.starts_with("schema ")
            || t.starts_with("schema{")
    })
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}
