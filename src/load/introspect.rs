//! Load a schema by introspection — from a live endpoint (POST the standard
//! introspection query) or a local introspection JSON dump. Both flatten the
//! `__schema` payload into the same [`SchemaRecord`]s that SDL produces.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::model::{Kind, SchemaRecord};

/// POST the introspection query to `url` and flatten the result.
pub fn from_url(url: &str) -> Result<Vec<SchemaRecord>> {
    let resp = ureq::post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_json(serde_json::json!({ "query": INTROSPECTION_QUERY }))
        .map_err(|e| anyhow!("introspecting {url}: {e}"))?;
    let body: Value = resp
        .into_json()
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

struct Roots {
    query: Option<String>,
    mutation: Option<String>,
    subscription: Option<String>,
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
            let field_kind = if roots.query.as_deref() == Some(name.as_str()) {
                Kind::Query
            } else if roots.mutation.as_deref() == Some(name.as_str()) {
                Kind::Mutation
            } else if roots.subscription.as_deref() == Some(name.as_str()) {
                Kind::Subscription
            } else {
                Kind::Field
            };
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
