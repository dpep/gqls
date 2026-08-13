//! Schema ingestion: a source string is an http(s) URL (introspected live), a
//! path to an SDL file, or a path to an introspection JSON dump. All three
//! produce the same [`SchemaRecord`] list, so nothing downstream cares which it
//! was. When no source is given,
//! [`discover`] finds a schema in the current directory tree.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::model::SchemaRecord;

pub mod discover_cache;
pub mod introspect;
pub mod record_cache;
pub mod sdl;

/// Options that shape loading. `headers` applies to URL introspection alone;
/// `refresh` applies to every source, since all of them are cached.
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

/// Directories holding someone else's code, never searched. A schema in here
/// describes a dependency's API rather than this project's, and `node_modules`
/// alone is often larger than the repo around it.
///
/// Dot-directories are skipped by the walk itself, so `.git` and `.venv` need
/// no entry.
const FOREIGN_DIRS: &[&str] = &["node_modules", "vendor", "venv", "__pycache__"];

/// Build output and other generated directories. Skipped on the way in — but
/// unlike [`FOREIGN_DIRS`] these hold *your* project's files, and a schema
/// generated into one is a real thing to want, so a search that finds nothing
/// anywhere else comes back for them (see [`locate`]).
///
/// Together these two lists are what honouring `.gitignore` would mostly buy,
/// without the dependency: measured over 197 repos, 12,060 of the 28,572
/// directories walked were git-ignored — 96% of those under a Python `venv`,
/// with `coverage` and `__pycache__` most of the rest.
const BUILD_DIRS: &[&str] = &["target", "tmp", "dist", "build", "coverage"];

const MAX_DEPTH: usize = 12;

/// How far a pass reaches: the first skips generated directories, the last
/// resort searches them too.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    Normal,
    IntoBuildDirs,
}

impl Reach {
    fn skips(self, name: &[u8]) -> bool {
        if FOREIGN_DIRS.iter().any(|n| n.as_bytes() == name) {
            return true;
        }
        self == Reach::Normal && BUILD_DIRS.iter().any(|n| n.as_bytes() == name)
    }
}

/// Find a schema when none was passed: walk the current directory tree for
/// schema *documents* (not operation docs). A `supergraph*` file wins outright
/// — it's the composed graph of a federated monorepo. Otherwise by tier:
/// `.graphqls`, then a `schema.*` file, then an introspection `.json`, then any
/// `.graphql`/`.gql` whose contents look like SDL. Ties break on depth, so the
/// nearest wins; other candidates elsewhere are reported.
///
/// The walk is the most expensive thing a warm query does, so its answer is
/// remembered per directory (see [`discover_cache`]); `refresh` re-walks.
pub fn discover(refresh: bool) -> Result<String> {
    let mut span = crate::profile::span("discover");
    let root = std::env::current_dir()?;
    if !refresh {
        if let Some(p) = discover_cache::load(&root) {
            span.note(|| "remembered".to_string());
            // Same line as the walk prints: which schema answered is worth
            // knowing whether or not it took a walk to work it out.
            crate::detail!("using schema {} (remembered)", rel(&root, &p));
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    let (mut candidates, seen, searched) = locate(&root);
    span.note(|| {
        format!(
            "{} dirs, {} files read, {} candidates",
            seen.dirs,
            seen.sniffed,
            candidates.len()
        )
    });
    drop(span);

    if candidates.is_empty() {
        bail!(
            "no GraphQL schema found under {} — pass a .graphql file or an http(s) URL",
            searched.display()
        );
    }
    let chosen = candidates.remove(0);

    // Counts the candidates confirmed, which is all of them only when the search
    // had to read everything. Under-reporting a hint is the right way round:
    // every file named here really is a schema.
    let elsewhere = candidates
        .iter()
        .filter(|c| c.path.parent() != chosen.path.parent())
        .count();
    crate::detail!("using schema {}", rel(&searched, &chosen.path));
    if elsewhere > 0 {
        crate::detail!("{elsewhere} other schema file(s) elsewhere — pass a path to pick one");
    }
    discover_cache::store(&root, &chosen.path);

    Ok(chosen.path.to_string_lossy().into_owned())
}

/// Search `dir` for schemas, best first, and say where the answer came from.
///
/// A directory with nothing under it falls back to the enclosing git repo:
/// `gqls user` run in `repo/src/components` should find the repo's schema
/// rather than report that this particular subdirectory has none. Searching
/// *down* from where you stand stays the rule — it's what lets a federated
/// subgraph resolve to its own schema — and the fallback only happens where
/// the alternative was an error.
fn locate(dir: &Path) -> (Vec<Candidate>, Scanned, PathBuf) {
    let mut seen = Scanned::default();
    let mut search = |from: &Path, reach: Reach| {
        let mut found = walk(from, 0, reach);
        resolve(&mut found);
        seen = std::mem::take(&mut seen).merge(found.seen);
        found.confirmed
    };

    let mut searched = dir.to_path_buf();
    let mut confirmed = search(dir, Reach::Normal);

    // Widen before giving up, cheapest step first.
    if confirmed.is_empty() {
        if let Some(root) = repo_root(dir).filter(|r| r != dir) {
            crate::detail!(
                "no schema under {} — searching the repo root {}",
                crate::paths::display(dir),
                crate::paths::display(&root)
            );
            confirmed = search(&root, Reach::Normal);
            searched = root;
        }
    }
    if confirmed.is_empty() {
        // Last resort: the generated directories skipped above. A schema
        // written by a build step is still the project's schema, and by now
        // the alternative is telling the user there isn't one.
        crate::detail!("still nothing — searching generated directories too");
        confirmed = search(&searched.clone(), Reach::IntoBuildDirs);
    }

    let mut candidates = confirmed;
    candidates.sort_by(|a, b| {
        // a `supergraph*` schema wins outright (the composed graph); otherwise
        // nearest-and-most-canonical by tier, then depth, then path.
        b.supergraph
            .cmp(&a.supergraph)
            .then(a.tier.cmp(&b.tier))
            .then(a.depth.cmp(&b.depth))
            .then(a.path.cmp(&b.path))
    });
    (candidates, seen, searched)
}

/// The git repository `dir` sits in, if any. `.git` is a directory in a normal
/// clone and a file in a worktree or submodule, so its kind isn't checked.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
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

/// What the walk had to touch to answer — the numbers that explain its cost.
#[derive(Default)]
struct Scanned {
    dirs: usize,
    /// Files whose contents were read to decide whether they're a schema.
    sniffed: usize,
}

impl Scanned {
    fn merge(mut self, other: Scanned) -> Scanned {
        self.dirs += other.dirs;
        self.sniffed += other.sniffed;
        self
    }
}

/// A file whose *name* allows it to be a schema, pending a look inside.
struct Maybe {
    path: PathBuf,
    depth: usize,
    tier: u8,
    supergraph: bool,
}

/// Everything one subtree found. Returned rather than pushed into a shared
/// buffer so the recursion can fan out across threads.
#[derive(Default)]
struct Found {
    /// Settled by name — a `.graphqls`, or a `schema.*`.
    confirmed: Vec<Candidate>,
    /// Still to be read, if it turns out to matter.
    maybes: Vec<Maybe>,
    seen: Scanned,
}

impl Found {
    fn merge(mut self, other: Found) -> Found {
        self.confirmed.extend(other.confirmed);
        self.maybes.extend(other.maybes);
        self.seen = self.seen.merge(other.seen);
        self
    }

    /// Read the maybes that `keep` selects, promoting the real schemas among
    /// them and discarding the rest.
    fn confirm(&mut self, keep: impl Fn(&Maybe) -> bool) {
        use rayon::prelude::*;

        let (to_read, rest): (Vec<Maybe>, Vec<Maybe>) = std::mem::take(&mut self.maybes)
            .into_iter()
            .partition(&keep);
        self.maybes = rest;
        self.seen.sniffed += to_read.len();
        let confirmed: Vec<Candidate> = to_read
            .into_par_iter()
            .filter(|m| match m.tier {
                2 => sniff_is_introspection(&m.path),
                _ => sniff_is_schema(&m.path),
            })
            .map(|m| Candidate {
                path: m.path,
                depth: m.depth,
                tier: m.tier,
                supergraph: m.supergraph,
            })
            .collect();
        self.confirmed.extend(confirmed);
    }
}

/// Settle which candidates actually are schemas, reading as few of them as the
/// answer allows. The ranking is `supergraph`, then tier, then depth — so a
/// `supergraph*` name is always worth reading, and once something is confirmed
/// nothing of a worse tier can beat it, however many files that leaves unread.
/// In a tree of many repos that's thousands of opens skipped.
fn resolve(found: &mut Found) {
    found.confirm(|m| m.supergraph);
    if found.confirmed.iter().any(|c| c.supergraph) {
        return;
    }
    for tier in [2, 3] {
        if found.confirmed.iter().any(|c| c.tier < tier) {
            return;
        }
        found.confirm(|m| m.tier == tier);
    }
}

/// Walk a subtree for schema candidates. Sibling directories are searched in
/// parallel: this is thousands of `read_dir` calls deep in a monorepo, all of
/// them waiting on the disk rather than the CPU, so the fan-out is close to
/// free. Rayon's work stealing handles the recursion — a lopsided tree doesn't
/// leave one thread walking alone.
///
/// The per-entry path is deliberately *not* built. A large tree hands us a
/// hundred thousand files that are obviously not schemas, and a `PathBuf` per
/// one costs more than the decision does; the name is enough to rule them out.
fn walk(dir: &Path, depth: usize, reach: Reach) -> Found {
    use rayon::prelude::*;

    if depth > MAX_DEPTH {
        return Default::default();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Default::default();
    };
    let mut found = Found {
        seen: Scanned {
            dirs: 1,
            sniffed: 0,
        },
        ..Default::default()
    };
    let mut subdirs = Vec::new();

    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        if ft.is_dir() {
            if name.starts_with(b".") || reach.skips(name) {
                continue;
            }
            subdirs.push(entry.path());
        } else if ft.is_file() {
            let supergraph = starts_with_ignore_case(name, b"supergraph");
            match classify(name) {
                Some(Verdict::Schema(tier)) => found.confirmed.push(Candidate {
                    path: entry.path(),
                    depth,
                    tier,
                    supergraph,
                }),
                // The name only narrows it to "could be". Whether it's worth
                // opening depends on what the rest of the tree turns up, so
                // that decision waits for the walk to finish.
                Some(Verdict::Sniff(tier)) => found.maybes.push(Maybe {
                    path: entry.path(),
                    depth,
                    tier,
                    supergraph,
                }),
                None => {}
            }
        }
    }

    let children = subdirs
        .par_iter()
        .map(|d| walk(d, depth + 1, reach))
        .reduce(Default::default, Found::merge);
    found.merge(children)
}

/// What a file's *name* says about it. Lower tier = more likely to be *the*
/// schema; a `Sniff` verdict still has to be confirmed by reading the file.
enum Verdict {
    Schema(u8),
    Sniff(u8),
}

/// The schema-source tier of a file by name alone, or `None` if the name rules
/// it out. Byte comparisons, no allocation: this runs on every file in the
/// tree, and all but a handful are rejected here.
fn classify(name: &[u8]) -> Option<Verdict> {
    if ends_with_ignore_case(name, b".graphqls") {
        return Some(Verdict::Schema(0));
    }
    let sdl_ext = ends_with_ignore_case(name, b".graphql") || ends_with_ignore_case(name, b".gql");
    if sdl_ext {
        if starts_with_ignore_case(name, b"schema.") {
            return Some(Verdict::Schema(1));
        }
        return Some(Verdict::Sniff(3));
    }
    if ends_with_ignore_case(name, b".json") {
        return Some(Verdict::Sniff(2));
    }
    None
}

fn ends_with_ignore_case(name: &[u8], suffix: &[u8]) -> bool {
    name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

fn starts_with_ignore_case(name: &[u8], prefix: &[u8]) -> bool {
    name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// How much of a `.json` file the introspection sniff reads. An actual dump
/// opens with `__schema`/`queryType`, so this is generous already — reading
/// further doesn't find more dumps, it just collects JSON that happens to
/// mention the word somewhere.
const SNIFF_JSON: u64 = 4 * 1024;

/// How much of a `.graphql` file the SDL sniff reads. More, because a schema's
/// first definition can sit below a licence header — but bounded, since the
/// alternative is reading every generated operation document in the tree to
/// its end.
const SNIFF_SDL: u64 = 64 * 1024;

/// The first `limit` bytes of a file as text, or empty if it can't be read.
///
/// Stops at the first byte that isn't UTF-8, which does two jobs: the read
/// boundary may land mid-character, and a file that isn't UTF-8 at all is one
/// the loader can't read either — sniffing it leniently would let discovery
/// choose a schema that then fails to load.
fn head(path: &Path, limit: u64) -> String {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    // `take` + `read_to_end` rather than one `read`: a single read is allowed
    // to come up short, which would sniff the first few hundred bytes and call
    // a schema an operation document.
    let _ = f.take(limit).read_to_end(&mut buf);
    match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(e) => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
    }
}

/// Cheap check that a `.json` file is a GraphQL introspection dump.
fn sniff_is_introspection(path: &Path) -> bool {
    let s = head(path, SNIFF_JSON);
    s.contains("__schema") || s.contains("\"queryType\"")
}

/// Cheap check that a `.graphql` file is SDL (type definitions) rather than an
/// operation document (`query`/`mutation` blocks) — avoids a full parse of the
/// many query files a project may carry.
fn sniff_is_schema(path: &Path) -> bool {
    head(path, SNIFF_SDL).lines().any(|l| {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gqls-walk-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tier_of(name: &str) -> Option<u8> {
        match classify(name.as_bytes())? {
            Verdict::Schema(t) | Verdict::Sniff(t) => Some(t),
        }
    }

    #[test]
    fn a_files_name_tiers_it_without_reading_it() {
        // Every file in the tree passes through here, so it settles what it can
        // from the name alone and only defers the ambiguous ones to a read.
        assert!(matches!(
            classify(b"api.graphqls"),
            Some(Verdict::Schema(0))
        ));
        assert!(matches!(
            classify(b"schema.graphql"),
            Some(Verdict::Schema(1))
        ));
        assert!(matches!(
            classify(b"query.graphql"),
            Some(Verdict::Sniff(3))
        ));
        assert!(matches!(classify(b"dump.json"), Some(Verdict::Sniff(2))));
        assert!(classify(b"main.rs").is_none());
        // extensions are matched case-insensitively, as before
        assert_eq!(tier_of("Schema.GraphQL"), Some(1));
        assert_eq!(tier_of("API.GRAPHQLS"), Some(0));
        // and a bare extension isn't a filename
        assert!(classify(b".graphql").is_none());
    }

    #[test]
    fn the_walk_finds_schemas_and_skips_the_noise() {
        // No `schema.*` here, so every candidate has to be read — which is
        // what puts the operation document and the plain JSON to the test.
        let root = scratch("finds");
        std::fs::write(root.join("ops.graphql"), "query Get { a }").unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"x"}"#).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(
            root.join("node_modules/pkg/schema.graphql"),
            "type Query { a: Int }",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/other.graphql"), "type Other { b: Int }").unwrap();

        let mut found = walk(&root, 0, Reach::Normal);
        resolve(&mut found);
        let paths: Vec<String> = found
            .confirmed
            .iter()
            .map(|c| c.path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        // the operation document and the package manifest are both rejected,
        // and node_modules — which holds a `schema.graphql` — is never entered
        assert_eq!(paths, ["sub/other.graphql"]);
        assert_eq!(found.seen.dirs, 2, "node_modules should not be walked");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_is_only_read_when_it_could_still_win() {
        // `schema.graphql` is settled by name and outranks both a `.json` dump
        // and a sniffed `.graphql`, so neither is opened.
        let root = scratch("lazy");
        std::fs::write(root.join("schema.graphql"), "type Query { a: Int }").unwrap();
        std::fs::write(root.join("dump.json"), r#"{"__schema":{}}"#).unwrap();
        std::fs::write(root.join("other.graphql"), "type Other { b: Int }").unwrap();

        let mut found = walk(&root, 0, Reach::Normal);
        assert_eq!(found.maybes.len(), 2);
        resolve(&mut found);
        assert_eq!(found.seen.sniffed, 0, "nothing needed reading");
        assert_eq!(found.confirmed.len(), 1);

        // with the named schema gone, the dump is worth reading — and settles
        // it before the sniffed SDL file is
        std::fs::remove_file(root.join("schema.graphql")).unwrap();
        let mut found = walk(&root, 0, Reach::Normal);
        resolve(&mut found);
        assert_eq!(found.seen.sniffed, 1, "only the .json");
        assert_eq!(found.confirmed.len(), 1);
        assert!(found.confirmed[0].path.ends_with("dump.json"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_subdirectory_with_no_schema_falls_back_to_the_repo() {
        // `gqls user` in repo/src/components should find the repo's schema,
        // not report that this particular subdirectory hasn't got one.
        let repo = scratch("fallback");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("schema.graphql"), "type Query { a: Int }").unwrap();
        let deep = repo.join("src/components");
        std::fs::create_dir_all(&deep).unwrap();

        let (found, _, searched) = locate(&deep);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("schema.graphql"));
        assert_eq!(searched, repo);

        // and searching *down* still wins where there's something to find
        std::fs::write(deep.join("local.graphql"), "type Local { b: Int }").unwrap();
        let (found, _, searched) = locate(&deep);
        assert!(
            found[0].path.ends_with("local.graphql"),
            "{found:?}",
            found = found[0].path
        );
        assert_eq!(searched, deep);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_generated_schema_is_found_only_when_nothing_else_is() {
        // Build directories are skipped on the way in — but a schema written
        // by a build step is still the project's schema, and "no schema found"
        // would be the wrong answer.
        let root = scratch("lastditch");
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/schema.graphql"), "type Query { a: Int }").unwrap();
        std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        std::fs::write(
            root.join("node_modules/dep/schema.graphql"),
            "type Dep { c: Int }",
        )
        .unwrap();

        let (found, _, _) = locate(&root);
        assert_eq!(found.len(), 1, "the dependency's schema is never ours");
        assert!(found[0].path.ends_with("build/schema.graphql"));

        // with a schema outside the build directory, that one wins and the
        // build directory is never entered
        std::fs::write(root.join("schema.graphql"), "type Query { b: Int }").unwrap();
        let (found, _, _) = locate(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, root.join("schema.graphql"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_supergraph_is_always_worth_opening() {
        // It outranks every tier, so it has to be read even when a `schema.*`
        // was already found by name.
        let root = scratch("supergraph");
        std::fs::write(root.join("schema.graphql"), "type Query { a: Int }").unwrap();
        std::fs::write(root.join("supergraph.graphql"), "type Query { b: Int }").unwrap();

        let mut found = walk(&root, 0, Reach::Normal);
        resolve(&mut found);
        assert_eq!(found.seen.sniffed, 1);
        assert!(found.confirmed.iter().any(|c| c.supergraph));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_not_a_schema() {
        // The loader reads with `read_to_string`, so discovering one of these
        // would choose a file that then fails to load.
        let root = scratch("utf8");
        let bad = root.join("weird.graphql");
        std::fs::write(&bad, b"type Query { a: Int }\n\xff\xfe not text").unwrap();
        assert!(sniff_is_schema(&bad));

        let worse = root.join("worse.graphql");
        std::fs::write(&worse, b"\xff\xfetype Query { a: Int }").unwrap();
        assert!(!sniff_is_schema(&worse), "invalid bytes come first");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sniff_reads_a_bounded_head() {
        let root = scratch("bounded");
        let p = root.join("huge.graphql");
        let mut body = "# comment\n".repeat(SNIFF_SDL as usize / 10 + 100);
        body.push_str("type Query { a: Int }\n");
        std::fs::write(&p, &body).unwrap();
        // the definition sits past the bound, so this file is not a schema as
        // far as discovery is concerned — the cost of the cap, stated
        assert!(!sniff_is_schema(&p));
        assert_eq!(head(&p, 16).len(), 16);
        let _ = std::fs::remove_dir_all(&root);
    }
}
