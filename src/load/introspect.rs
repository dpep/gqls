//! Load a schema by introspection — from a live endpoint (POST the standard
//! introspection query) or a local introspection JSON dump. Both flatten the
//! `__schema` payload into the same [`SchemaRecord`]s that SDL produces.

use std::collections::BTreeMap;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::LoadOptions;
use crate::model::{Kind, Roots, SchemaRecord};

/// Overall per-request deadline for live introspection (connect + read) — a
/// hung or slow endpoint fails instead of blocking gqls forever.
const TIMEOUT_SECS: u64 = 30;
/// Default introspection-response cache lifetime for remote endpoints; see [`ttl`].
const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

/// Age at which a cached response is deleted rather than merely ignored. The
/// TTL decides what's *servable*; this bounds what accumulates, since an
/// endpoint queried once leaves a file (often megabytes) behind forever.
const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cached responses to keep, least-recently-used evicted first. Room for a
/// handful of endpoints, each under a few credentials, without letting a
/// rotating token accumulate a file per run for a week.
const MAX_FILES: usize = 16;

/// Total bytes of cached responses to retain. A file count can't bound disk on
/// its own: one introspection body here measured 5.7MB, and schemas only grow.
const MAX_BYTES: u64 = 100 * 1024 * 1024;

/// POST the introspection query to `url` and flatten the result. Honors
/// `opts.headers` (e.g. an `Authorization` token) and a TTL response cache
/// (1h for remote endpoints, never for localhost) so repeated queries against a
/// remote endpoint don't refetch all day.
pub(crate) fn from_url(url: &str, opts: &LoadOptions) -> Result<Vec<SchemaRecord>> {
    let ttl = ttl(url);
    // A zero TTL (localhost, or GQLS_INTROSPECT_TTL=0) means no caching at all —
    // neither read nor write, so a schema you're actively editing is never stale.
    let path = (!ttl.is_zero())
        .then(|| cache_path(url, &opts.headers))
        .flatten();

    if !opts.refresh {
        if let Some(p) = path.as_deref() {
            if let Some(bytes) = read_if_fresh(p, ttl) {
                match records_from(&bytes, url, opts.refresh) {
                    Ok(records) => {
                        crate::detail!("introspection cache hit: {}", crate::paths::display(p));
                        return Ok(records);
                    }
                    // A cached body that no longer yields a schema is a miss, not
                    // a failure: bailing here would make one bad file poison every
                    // run for the rest of the TTL, with `--refresh` the only way
                    // out and no hint that it's needed.
                    Err(e) => {
                        crate::detail!("cached response unusable ({e}) — refetching");
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }
    }

    crate::detail!("introspecting {url}");
    // Timed apart from the parse below, because for a remote source the network
    // is usually most of the wall clock and the least controllable part of it —
    // one undifferentiated `load` span can't tell a slow endpoint from slow gqls.
    let mut fetch_span = crate::profile::span("introspect: fetch");
    let bytes = fetch(url, &opts.headers)?;
    fetch_span.note(|| format!("{:.1} KB", bytes.len() as f64 / 1024.0));
    drop(fetch_span);
    // Parse and validate *before* caching. Servers answer 200 with an `errors`
    // body for an expired token or disabled introspection; caching that would
    // turn a transient failure into an hour of them.
    let records = records_from(&bytes, url, opts.refresh)?;
    if let Some(p) = path.as_deref() {
        store_response(p, &bytes);
    }
    Ok(records)
}

/// Records from a raw introspection payload, or an error if it isn't one.
/// This is the validation gate: nothing reaches the cache without passing it.
/// Accepts the shapes a dump is written in — `{data:{__schema}}`, `{__schema}`,
/// or the bare schema object.
///
/// `source` is the URL or the file path — a saved response is the same bytes a
/// server sent, so both go through here and get the same answer.
pub(super) fn records_from(raw: &[u8], source: &str, refresh: bool) -> Result<Vec<SchemaRecord>> {
    // The parsed records depend only on the response bytes, so the record
    // cache short-circuits the (large) JSON parse on repeat queries. One exit,
    // so the disclosure below is not something a cache hit skips.
    let records = match (!refresh).then(|| super::record_cache::load(raw)).flatten() {
        Some(records) => records,
        None => {
            let mut parse_span = crate::profile::span("introspect: parse");
            let body: Value = serde_json::from_slice(raw)
                .with_context(|| format!("parsing introspection response from {source}"))?;
            let records = from_introspection(schema_of(&body, source)?)?;
            parse_span.note(|| format!("{} records", records.len()));
            drop(parse_span);
            super::record_cache::store(raw, &records);
            records
        }
    };
    note_undisclosed_directives(&records);
    Ok(records)
}

/// Directives every server defines whether the schema uses them or not, so a
/// definition is no evidence that anything was hidden: `@skip`/`@include` apply
/// to operations rather than to the schema, `@deprecated` arrives by its own
/// side channel (`isDeprecated`), and `@specifiedBy`/`@oneOf` come with the
/// built-in scalars. GitHub's 11k-record dump defines exactly these five.
const UNREMARKABLE_DIRECTIVES: &[&str] = &["skip", "include", "deprecated", "specifiedBy", "oneOf"];

/// Say, under `-v`, that this source can't answer "what's applied here".
///
/// Standard introspection exposes directive *definitions* but not their
/// applications, so every record's `directives` comes back empty — which reads
/// exactly like a schema that applies none. The schema defining directives of
/// its own is the evidence that the difference matters; without that, the
/// silence is accurate and there is nothing to disclose.
///
/// A `-v` line rather than a status one: it would otherwise print on every run
/// against a federated endpoint, where `@key` is on half the types.
fn note_undisclosed_directives(records: &[SchemaRecord]) {
    if !crate::logging::is_verbose() {
        return;
    }
    let defined: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == Kind::Directive)
        .map(|r| r.name.as_str())
        .filter(|n| !UNREMARKABLE_DIRECTIVES.contains(n))
        .collect();
    let Some((first, rest)) = defined.split_first() else {
        return;
    };
    let named = match rest.len() {
        0 => format!("@{first}"),
        n => format!("@{first} (+{n} more)"),
    };
    crate::detail!(
        "introspection reports no applied directives — this schema defines {named}, \
         but not where it's used; its SDL says that"
    );
}

/// The `__schema` object inside an introspection payload — under `data` as a
/// server answers, or hoisted to the top level (or unwrapped entirely) as the
/// tools that save dumps write it.
fn schema_of<'a>(body: &'a Value, source: &str) -> Result<&'a Value> {
    // Only a non-empty errors array is a real failure — many servers send
    // `"errors": null` or `[]` alongside a valid `data`.
    if let Some(errors) = body
        .get("errors")
        .filter(|e| !e.is_null() && !is_empty_array(e))
    {
        bail!("introspection returned errors: {errors}");
    }
    let schema = body
        .pointer("/data/__schema")
        .or_else(|| body.get("__schema"))
        .unwrap_or(body);
    if schema.get("types").is_none() {
        bail!("{source} is not a GraphQL introspection dump (no __schema.types)");
    }
    Ok(schema)
}

/// Cache a validated response, and drop long-expired ones while we're here —
/// nothing else ever deletes them, and introspection bodies run to megabytes.
/// Written via a temp file and renamed, so a concurrent reader either sees the
/// previous response or this one, never half of either.
fn store_response(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    prune(dir, MAX_FILES, MAX_BYTES);
}

/// Drop expired responses, then the least-recently-used ones past either
/// budget. Age alone stopped bounding this when credentials entered the key: an
/// endpoint queried with a rotating token — CI with short-lived credentials —
/// now leaves one megabytes-sized file per token until they age out a week
/// later. Two limits because a count can't bound disk when file size follows
/// schema size. The newest always survives, since evicting what was just
/// written guarantees an immediate refetch.
fn prune(dir: &Path, keep: usize, max_bytes: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                let modified = m.modified().ok()?;
                // Expired is expired, whatever the budgets say.
                if modified.elapsed().is_ok_and(|d| d > STALE_AFTER) {
                    let _ = std::fs::remove_file(e.path());
                    return None;
                }
                Some((modified, m.len(), e.path()))
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

fn fetch(url: &str, headers: &[(String, String)]) -> Result<Vec<u8>> {
    let mut req = ureq::post(url)
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json");
    for (name, value) in headers {
        req = req.set(name, value);
    }
    let resp = req
        .send_json(serde_json::json!({ "query": INTROSPECTION_QUERY }))
        .map_err(|e| anyhow!("introspecting {url}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .with_context(|| format!("reading introspection response from {url}"))?;
    Ok(buf)
}

/// Cache file for a URL's introspection response, keyed by the URL *and* the
/// request headers. The same endpoint answers differently per credential —
/// multi-tenant, or prod and staging behind one hostname — so a key that saw
/// only the URL served the first caller's schema to every later one, whatever
/// token it presented (or didn't).
fn cache_path(url: &str, headers: &[(String, String)]) -> Option<PathBuf> {
    Some(cache_dir()?.join(cache_file_name(cache_key(url, headers))))
}

/// Bump when the recipe below changes: entries written under the old rule then
/// become unreachable instead of being served under the new one. Bumped to 2
/// when headers entered the key — a pre-2 entry may hold a credentialed schema
/// that an unauthenticated run would otherwise still be handed.
const KEY_VERSION: u32 = 2;

fn cache_key(url: &str, headers: &[(String, String)]) -> u64 {
    let mut h = DefaultHasher::new();
    KEY_VERSION.hash(&mut h);
    url.hash(&mut h);
    // Sorted, so `-H a -H b` and `-H b -H a` share an entry; lowercased, because
    // HTTP header names are case-insensitive. Duplicates are kept rather than
    // collapsed: ureq's own last-wins rule has exceptions, and an extra fetch is
    // the safe direction to be wrong in — a wrong *hit* is the bug being fixed.
    let mut canonical: Vec<(String, &str)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.as_str()))
        .collect();
    canonical.sort();
    canonical.hash(&mut h);
    h.finish()
}

/// The key as a filename. Hashed, never spelled out: a header value is often a
/// token, and a cache path is readable by anything that can list the directory.
fn cache_file_name(key: u64) -> String {
    format!("{key:016x}.json")
}

fn cache_dir() -> Option<PathBuf> {
    Some(crate::paths::cache_dir()?.join("introspect"))
}

/// The cached bytes if the file is younger than `ttl`, else `None`.
fn read_if_fresh(path: &Path, ttl: Duration) -> Option<Vec<u8>> {
    let age = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    (age <= ttl).then(|| std::fs::read(path).ok()).flatten()
}

/// Effective cache lifetime for `url`: `GQLS_INTROSPECT_TTL` (seconds) if set,
/// else zero for localhost — you're likely editing that schema, so always fetch
/// fresh — and [`DEFAULT_TTL`] (1h) for remote endpoints.
fn ttl(url: &str) -> Duration {
    if let Some(secs) = std::env::var("GQLS_INTROSPECT_TTL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Duration::from_secs(secs);
    }
    if is_localhost(url) {
        return Duration::ZERO;
    }
    DEFAULT_TTL
}

/// Whether `url` points at the local machine (loopback), where the schema is
/// probably under active development and should never be served from cache.
fn is_localhost(url: &str) -> bool {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // Pull the host out of `host:port` / `[ipv6]:port` / bare host.
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host == "0.0.0.0"
        || host.starts_with("127.")
}

/// Delete all cached introspection responses; returns how many were removed.
pub(crate) fn clear_cache() -> usize {
    let Some(dir) = cache_dir() else { return 0 };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

fn from_introspection(schema: &Value) -> Result<Vec<SchemaRecord>> {
    let roots = Roots {
        query: root_name(schema, "queryType"),
        mutation: root_name(schema, "mutationType"),
        subscription: root_name(schema, "subscriptionType"),
    };

    let mut out = Vec::new();
    for t in array(schema, "types") {
        emit_type(t, &roots, &mut out);
    }
    for d in array(schema, "directives") {
        let name = str_field(d, "name");
        if name.is_empty() {
            continue;
        }
        out.push(SchemaRecord {
            path: format!("@{name}"),
            name,
            kind: Kind::Directive,
            parent: None,
            type_ref: None,
            args: args_of(d),
            arg_descriptions: arg_docs_of(d),
            description: opt_str(d, "description"),
            deprecated: None,
            directives: Vec::new(),
            default: None,
            possible_types: Vec::new(),
        });
    }
    Ok(out)
}

fn root_name(schema: &Value, key: &str) -> Option<String> {
    schema.get(key)?.get("name")?.as_str().map(str::to_string)
}

fn emit_type(t: &Value, roots: &Roots, out: &mut Vec<SchemaRecord>) {
    let name = str_field(t, "name");
    if name.is_empty() || name.starts_with("__") {
        return; // skip introspection meta types (__Type, __Schema, ...)
    }
    let type_kind = match str_field(t, "kind").as_str() {
        "OBJECT" => Kind::Object,
        "INTERFACE" => Kind::Interface,
        "UNION" => Kind::Union,
        "ENUM" => Kind::Enum,
        "INPUT_OBJECT" => Kind::InputObject,
        "SCALAR" => Kind::Scalar,
        _ => return,
    };

    out.push(SchemaRecord {
        path: name.clone(),
        name: name.clone(),
        kind: type_kind,
        parent: None,
        type_ref: None,
        args: Vec::new(),
        arg_descriptions: Default::default(),
        description: opt_str(t, "description"),
        deprecated: None,
        directives: Vec::new(),
        // Introspection reports a union's members and an interface's
        // implementors the same way, so both arrive here.
        default: None,
        possible_types: array(t, "possibleTypes")
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
    });

    match type_kind {
        Kind::Object | Kind::Interface => {
            let field_kind = roots.field_kind(&name);
            for f in array(t, "fields") {
                let fname = str_field(f, "name");
                out.push(SchemaRecord {
                    path: format!("{name}.{fname}"),
                    name: fname,
                    kind: field_kind,
                    parent: Some(name.clone()),
                    type_ref: f.get("type").map(render_type),
                    args: args_of(f),
                    arg_descriptions: arg_docs_of(f),
                    description: opt_str(f, "description"),
                    deprecated: deprecation(f),
                    directives: Vec::new(),
                    default: None,
                    possible_types: Vec::new(),
                });
            }
        }
        Kind::InputObject => {
            for f in array(t, "inputFields") {
                let fname = str_field(f, "name");
                out.push(SchemaRecord {
                    path: format!("{name}.{fname}"),
                    name: fname,
                    kind: Kind::InputField,
                    parent: Some(name.clone()),
                    type_ref: f.get("type").map(render_type),
                    args: Vec::new(),
                    arg_descriptions: Default::default(),
                    description: opt_str(f, "description"),
                    deprecated: deprecation(f),
                    directives: Vec::new(),
                    // Arrives as a GraphQL literal in a string, same as an
                    // argument's — see `args_of`.
                    default: opt_str(f, "defaultValue"),
                    possible_types: Vec::new(),
                });
            }
        }
        Kind::Enum => {
            for v in array(t, "enumValues") {
                let vname = str_field(v, "name");
                out.push(SchemaRecord {
                    path: format!("{name}.{vname}"),
                    name: vname,
                    kind: Kind::EnumValue,
                    parent: Some(name.clone()),
                    type_ref: None,
                    args: Vec::new(),
                    arg_descriptions: Default::default(),
                    description: opt_str(v, "description"),
                    deprecated: deprecation(v),
                    directives: Vec::new(),
                    default: None,
                    possible_types: Vec::new(),
                });
            }
        }
        _ => {}
    }
}

/// Render an introspection type-ref (the `ofType` chain) like SDL: `[User!]!`.
fn render_type(t: &Value) -> String {
    match t.get("kind").and_then(Value::as_str) {
        Some("NON_NULL") => format!("{}!", t.get("ofType").map(render_type).unwrap_or_default()),
        Some("LIST") => format!("[{}]", t.get("ofType").map(render_type).unwrap_or_default()),
        _ => t
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
    }
}

fn args_of(f: &Value) -> Vec<String> {
    array(f, "args")
        .iter()
        .map(|a| {
            let n = str_field(a, "name");
            let ty = a.get("type").map(render_type).unwrap_or_default();
            // `defaultValue` arrives as a GraphQL literal in a string.
            match a.get("defaultValue").and_then(Value::as_str) {
                Some(default) => format!("{n}: {ty} = {default}"),
                None => format!("{n}: {ty}"),
            }
        })
        .collect()
}

/// What each documented argument is for. The introspection query has always
/// asked for these — they were parsed and dropped.
fn arg_docs_of(f: &Value) -> BTreeMap<String, String> {
    array(f, "args")
        .iter()
        // `opt_str`, so an empty description is absent here exactly as it is on
        // the record itself — servers send `""` for an undocumented argument.
        .filter_map(|a| Some((str_field(a, "name"), opt_str(a, "description")?)))
        .collect()
}

fn deprecation(v: &Value) -> Option<String> {
    (v.get("isDeprecated").and_then(Value::as_bool) == Some(true)).then(|| {
        opt_str(v, "deprecationReason")
            .unwrap_or_else(|| super::DEFAULT_DEPRECATION_REASON.to_string())
    })
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn is_empty_array(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.is_empty())
}

/// The standard introspection query (7-level `ofType` nesting covers any
/// realistic list/non-null wrapping).
const INTROSPECTION_QUERY: &str = r#"
query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
    directives { name description args { ...InputValue } }
  }
}
fragment FullType on __Type {
  kind name description
  possibleTypes { kind name }
  fields(includeDeprecated: true) {
    name description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated deprecationReason
  }
  inputFields { ...InputValue }
  enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason }
}
fragment InputValue on __InputValue { name description defaultValue type { ...TypeRef } }
fragment TypeRef on __Type {
  kind name
  ofType { kind name ofType { kind name ofType { kind name ofType { kind name
  ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}
"#;

#[cfg(test)]
mod tests {
    use super::{cache_file_name, cache_key, is_localhost, records_from};

    const URL: &str = "https://api.example.com/graphql";

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn credentials_key_the_introspection_cache() {
        // The bug: keyed on the URL alone, the first response was replayed for
        // every later run whatever token it passed — a revoked token kept
        // working, and two tenants behind one URL saw each other's schema.
        let good = cache_key(URL, &headers(&[("X-Token", "secret")]));
        let wrong = cache_key(URL, &headers(&[("X-Token", "WRONG")]));
        let none = cache_key(URL, &[]);
        assert_ne!(good, wrong);
        assert_ne!(good, none);
        assert_ne!(wrong, none);

        // An unauthenticated endpoint is the common path and still caches.
        assert_eq!(none, cache_key(URL, &[]));
        // ...and the URL still matters when the headers match.
        assert_ne!(none, cache_key("https://api.example.com/other", &[]));
    }

    #[test]
    fn the_same_credentials_hit_the_same_entry_however_they_are_written() {
        // `-H` is repeatable, and HTTP header names are case-insensitive, so
        // neither the order nor the spelling should cost a refetch.
        let a = cache_key(
            URL,
            &headers(&[("X-Token", "secret"), ("Authorization", "Bearer t")]),
        );
        let reordered = cache_key(
            URL,
            &headers(&[("Authorization", "Bearer t"), ("X-Token", "secret")]),
        );
        let recased = cache_key(
            URL,
            &headers(&[("AUTHORIZATION", "Bearer t"), ("x-token", "secret")]),
        );
        assert_eq!(a, reordered);
        assert_eq!(a, recased);
        // Values are not case-folded: tokens are case-sensitive.
        assert_ne!(a, cache_key(URL, &headers(&[("X-Token", "SECRET")])));
    }

    #[test]
    fn pruning_keeps_the_newest_within_both_budgets() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!("gqls-prune-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        // Five files, newest first by name: 0 is the most recent.
        for i in 0..5u64 {
            let p = dir.join(format!("{i:016x}.json"));
            std::fs::write(&p, vec![b'x'; 100]).expect("writing a cache file");
            let age = Duration::from_secs(60 * (i + 1));
            std::fs::File::open(&p)
                .expect("reopening")
                .set_modified(SystemTime::now() - age)
                .expect("backdating");
        }

        super::prune(&dir, 3, u64::MAX);
        let left = |d: &std::path::Path| {
            let mut names: Vec<String> = std::fs::read_dir(d)
                .expect("listing")
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(
            left(&dir),
            vec![
                "0000000000000000.json",
                "0000000000000001.json",
                "0000000000000002.json"
            ],
            "the three most recent should survive"
        );

        // A byte budget below one file's size still keeps that one file: the
        // alternative is evicting what was just written and refetching it.
        super::prune(&dir, 3, 10);
        assert_eq!(left(&dir), vec!["0000000000000000.json"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_is_not_readable_from_the_cache_path() {
        let name = cache_file_name(cache_key(URL, &headers(&[("X-Token", "secret")])));
        assert!(!name.contains("secret"), "{name}");
        let stem = name.strip_suffix(".json").expect("a .json file");
        assert!(
            stem.len() == 16 && stem.chars().all(|c| c.is_ascii_hexdigit()),
            "the file name should be nothing but the hash: {name}"
        );
    }

    #[test]
    fn an_argument_description_survives_the_introspection_loader() {
        // The introspection query has always asked for these; they were parsed
        // and thrown away. The SDL loader keeps them, and two loaders that
        // disagree about the same schema are the bug this guards.
        let dump = br#"{"data":{"__schema":{"queryType":{"name":"Query"},
            "types":[{"kind":"OBJECT","name":"Query","fields":[
              {"name":"repository","description":null,
               "args":[{"name":"owner","description":"The login of a user.",
                        "defaultValue":null,"type":{"kind":"SCALAR","name":"String"}},
                       {"name":"name","description":null,
                        "defaultValue":null,"type":{"kind":"SCALAR","name":"String"}},
                       {"name":"ref","description":"",
                        "defaultValue":null,"type":{"kind":"SCALAR","name":"String"}}],
               "type":{"kind":"SCALAR","name":"String"},"isDeprecated":false}]}]}}}"#;
        let records = records_from(dump, "http://x/graphql", true).expect("should load");
        let field = records
            .iter()
            .find(|r| r.path == "Query.repository")
            .expect("the field should load");
        assert_eq!(
            field.arg_descriptions.get("owner").map(String::as_str),
            Some("The login of a user.")
        );
        // an argument the schema doesn't document simply isn't there — `null`
        // and `""` are both "undocumented", and servers send both
        assert!(!field.arg_descriptions.contains_key("name"));
        assert!(!field.arg_descriptions.contains_key("ref"));
    }

    #[test]
    fn a_response_that_is_not_a_schema_is_rejected_before_it_can_be_cached() {
        // Servers answer 200 with an errors body for an expired token or
        // introspection turned off. `from_url` only caches what survives this,
        // so a bad response can't wedge the cache for the rest of the TTL.
        let errors = br#"{"errors":[{"message":"introspection is disabled"}]}"#;
        assert!(records_from(errors, "http://x/graphql", true).is_err());

        let no_schema = br#"{"data":{}}"#;
        assert!(records_from(no_schema, "http://x/graphql", true).is_err());

        assert!(records_from(b"not json at all", "http://x/graphql", true).is_err());

        // an empty errors array alongside real data is not a failure
        let ok = br#"{"errors":[],"data":{"__schema":{"types":[]}}}"#;
        assert!(records_from(ok, "http://x/graphql", true).is_ok());
    }

    #[test]
    fn a_saved_response_is_read_the_same_way_a_live_one_is() {
        // `curl … > schema.json` with an expired token saves a well-formed
        // record of a failed introspection. Telling the user the file format is
        // wrong sends them to fix the wrong thing.
        let failed = br#"{"data": null, "errors": [{"message": "introspection is disabled"}]}"#;
        let err = records_from(failed, "schema.json", true)
            .expect_err("a recorded failure is not a schema")
            .to_string();
        assert!(err.contains("introspection is disabled"), "{err}");

        // the shorthand shapes a dump is written in still load
        assert!(records_from(br#"{"__schema":{"types":[]}}"#, "hoisted.json", true).is_ok());
        assert!(records_from(br#"{"types":[]}"#, "bare.json", true).is_ok());

        // and JSON that is simply not a dump says so, naming the file
        let err = records_from(br#"{"name":"x"}"#, "package.json", true)
            .expect_err("not a dump")
            .to_string();
        assert!(err.contains("package.json"), "{err}");
    }

    #[test]
    fn localhost_urls_are_detected() {
        for url in [
            "http://localhost:4000/graphql",
            "http://127.0.0.1:8080/",
            "http://127.0.0.2/graphql",
            "http://[::1]:4000/graphql",
            "http://0.0.0.0:3000",
            "https://api.localhost/graphql",
            "http://user:pass@localhost:4000/",
        ] {
            assert!(is_localhost(url), "{url} should be localhost");
        }
    }

    #[test]
    fn remote_urls_are_not_localhost() {
        for url in [
            "https://countries.trevorblades.com/",
            "https://api.github.com/graphql",
            "https://mylocalhost.com/graphql",
            "http://localhost.evil.com/graphql",
        ] {
            assert!(!is_localhost(url), "{url} should not be localhost");
        }
    }
}
