//! Load a schema by introspection — from a live endpoint (POST the standard
//! introspection query) or a local introspection JSON dump. Both flatten the
//! `__schema` payload into the same [`SchemaRecord`]s that SDL produces.

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

/// POST the introspection query to `url` and flatten the result. Honors
/// `opts.headers` (e.g. an `Authorization` token) and a TTL response cache
/// (1h for remote endpoints, never for localhost) so repeated queries against a
/// remote endpoint don't refetch all day.
pub fn from_url(url: &str, opts: &LoadOptions) -> Result<Vec<SchemaRecord>> {
    let raw = fetch_or_cached(url, opts)?;
    let body: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("parsing introspection response from {url}"))?;

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
        .ok_or_else(|| anyhow!("no data.__schema in response from {url}"))?;
    from_introspection(schema)
}

/// The raw introspection response for `url` — a cached copy if one exists and is
/// younger than the [`ttl`], otherwise a fresh POST (which is then cached).
/// `opts.refresh` skips the cache read and forces a live fetch.
fn fetch_or_cached(url: &str, opts: &LoadOptions) -> Result<Vec<u8>> {
    let ttl = ttl(url);
    // A zero TTL (localhost, or GQLS_INTROSPECT_TTL=0) means no caching at all —
    // neither read nor write, so a schema you're actively editing is never stale.
    let path = (!ttl.is_zero()).then(|| cache_path(url)).flatten();
    if !opts.refresh {
        if let Some(p) = path.as_deref() {
            if let Some(bytes) = read_if_fresh(p, ttl) {
                crate::detail!("introspection cache hit: {}", p.display());
                return Ok(bytes);
            }
        }
    }
    crate::detail!("introspecting {url} (live)");
    let bytes = fetch(url, &opts.headers)?;
    if let Some(p) = path.as_deref() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, &bytes);
    }
    Ok(bytes)
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

/// Cache file for a URL's introspection response (keyed by the URL).
fn cache_path(url: &str) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    Some(cache_dir()?.join(format!("{:016x}.json", h.finish())))
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
pub fn clear_cache() -> usize {
    let Some(dir) = cache_dir() else { return 0 };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Load a local introspection JSON dump — accepts `{data:{__schema}}`,
/// `{__schema}`, or the bare schema object.
pub fn from_json_file(path: &str) -> Result<Vec<SchemaRecord>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("parsing {path}"))?;
    let schema = v
        .pointer("/data/__schema")
        .or_else(|| v.get("__schema"))
        .unwrap_or(&v);
    if schema.get("types").is_none() {
        bail!("{path} is not a GraphQL introspection dump (no __schema.types)");
    }
    from_introspection(schema)
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
            description: opt_str(d, "description"),
            deprecated: None,
            directives: Vec::new(),
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
        description: opt_str(t, "description"),
        deprecated: None,
        directives: Vec::new(),
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
                    description: opt_str(f, "description"),
                    deprecated: deprecation(f),
                    directives: Vec::new(),
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
                    description: opt_str(f, "description"),
                    deprecated: deprecation(f),
                    directives: Vec::new(),
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
                    description: opt_str(v, "description"),
                    deprecated: deprecation(v),
                    directives: Vec::new(),
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
            format!("{n}: {ty}")
        })
        .collect()
}

fn deprecation(v: &Value) -> Option<String> {
    (v.get("isDeprecated").and_then(Value::as_bool) == Some(true))
        .then(|| opt_str(v, "deprecationReason").unwrap_or_else(|| "deprecated".into()))
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
  fields(includeDeprecated: true) {
    name description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated deprecationReason
  }
  inputFields { ...InputValue }
  enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason }
}
fragment InputValue on __InputValue { name description type { ...TypeRef } }
fragment TypeRef on __Type {
  kind name
  ofType { kind name ofType { kind name ofType { kind name ofType { kind name
  ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}
"#;

#[cfg(test)]
mod tests {
    use super::is_localhost;

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
