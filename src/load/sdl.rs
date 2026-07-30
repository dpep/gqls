//! Parse GraphQL SDL and flatten every definition into [`SchemaRecord`]s.

use anyhow::{anyhow, Result};
use graphql_parser::schema::{
    parse_schema, Definition, Directive, EnumValue, Field, InputValue, Type, TypeDefinition,
    TypeExtension, Value,
};

use crate::model::{Kind, Roots, SchemaRecord};

/// The root operation names default to the conventional `Query`/`Mutation`/
/// `Subscription` when no `schema { ... }` block overrides them.
fn default_roots() -> Roots {
    Roots {
        query: Some("Query".into()),
        mutation: Some("Mutation".into()),
        subscription: Some("Subscription".into()),
    }
}

/// graphql-parser (0.4) doesn't parse GraphQL *schema extensions*
/// (`extend schema <directives> [block]`) — the header of every Apollo
/// Federation v2 subgraph file (`extend schema @link(url: "…", import: […])`).
/// They apply only federation directives (no searchable types), so we remove
/// them before parsing. Borrowed (no-op) when there are none.
fn strip_schema_extensions(sdl: &str) -> std::borrow::Cow<'_, str> {
    if !sdl.contains("extend") {
        return std::borrow::Cow::Borrowed(sdl);
    }
    let b = sdl.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] == b'e' {
            if let Some(end) = extend_schema_block(b, i) {
                out.push_str(&sdl[copied..i]);
                i = end;
                copied = end;
                continue;
            }
        }
        i += 1;
    }
    if copied == 0 {
        return std::borrow::Cow::Borrowed(sdl);
    }
    out.push_str(&sdl[copied..]);
    std::borrow::Cow::Owned(out)
}

/// If a schema extension (`extend schema <directives> [block]`) starts at `i`,
/// return the byte index just past it; else `None`.
fn extend_schema_block(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && is_ident(b[i - 1]) {
        return None; // not at a word boundary
    }
    let after_extend = keyword(b, i, b"extend")?;
    let after_ws = skip_trivia(b, after_extend);
    if after_ws == after_extend {
        return None; // need whitespace between `extend` and `schema`
    }
    let mut j = keyword(b, after_ws, b"schema")?;
    if b.get(j).is_some_and(|&c| is_ident(c)) {
        return None; // `schematic`, not `schema`
    }
    // consume applied directives, then an optional operation-types block
    loop {
        let k = skip_trivia(b, j);
        match b.get(k) {
            Some(b'@') => {
                let name_end = skip_ident(b, skip_trivia(b, k + 1));
                let args = skip_trivia(b, name_end);
                j = if b.get(args) == Some(&b'(') {
                    skip_balanced(b, args, b'(', b')')
                } else {
                    name_end
                };
            }
            Some(b'{') => return Some(skip_balanced(b, k, b'{', b'}')),
            _ => return Some(k),
        }
    }
}

fn keyword(b: &[u8], i: usize, kw: &[u8]) -> Option<usize> {
    b.get(i..i + kw.len())
        .filter(|s| *s == kw)
        .map(|_| i + kw.len())
}

fn is_ident(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

fn skip_ident(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(|&c| is_ident(c)) {
        i += 1;
    }
    i
}

/// Skip whitespace, commas (insignificant in GraphQL), and `#` line comments.
fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        while b
            .get(i)
            .is_some_and(|&c| c.is_ascii_whitespace() || c == b',')
        {
            i += 1;
        }
        if b.get(i) == Some(&b'#') {
            while b.get(i).is_some_and(|&c| c != b'\n') {
                i += 1;
            }
        } else {
            return i;
        }
    }
}

/// Skip a balanced `open`/`close` region starting at `i` (which is at `open`),
/// honoring string literals. Returns the index just past the matching close.
fn skip_balanced(b: &[u8], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 0usize;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            i = skip_string(b, i);
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    i
}

/// Skip a `"..."` or `"""..."""` string starting at `i`; return the index past it.
fn skip_string(b: &[u8], i: usize) -> usize {
    if b.get(i..i + 3) == Some(b"\"\"\"".as_slice()) {
        let mut j = i + 3;
        while j + 3 <= b.len() {
            if &b[j..j + 3] == b"\"\"\"" {
                return j + 3;
            }
            j += 1;
        }
        return b.len();
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

pub fn from_sdl(text: &str) -> Result<Vec<SchemaRecord>> {
    // graphql-parser (0.4) can't parse schema extensions (`extend schema
    // @link(...)`), which head every Apollo Federation v2 subgraph file, so
    // strip them first — they apply only federation directives, no types.
    let text = strip_schema_extensions(text);
    let doc = parse_schema::<String>(&text).map_err(|e| anyhow!("parsing SDL: {e}"))?;

    let mut roots = default_roots();
    for def in &doc.definitions {
        if let Definition::SchemaDefinition(s) = def {
            if let Some(q) = &s.query {
                roots.query = Some(q.clone());
            }
            if let Some(m) = &s.mutation {
                roots.mutation = Some(m.clone());
            }
            if let Some(sub) = &s.subscription {
                roots.subscription = Some(sub.clone());
            }
        }
    }

    let mut out = Vec::new();
    for def in &doc.definitions {
        match def {
            Definition::TypeDefinition(td) => emit_type(td, &roots, &mut out),
            Definition::DirectiveDefinition(d) => out.push(SchemaRecord {
                path: format!("@{}", d.name),
                name: d.name.clone(),
                kind: Kind::Directive,
                parent: None,
                type_ref: None,
                args: d.arguments.iter().map(fmt_input).collect(),
                description: d.description.clone(),
                deprecated: None,
                directives: Vec::new(),
            }),
            Definition::TypeExtension(te) => emit_type_extension(te, &roots, &mut out),
            _ => {}
        }
    }
    Ok(out)
}

fn emit_type(td: &TypeDefinition<'_, String>, roots: &Roots, out: &mut Vec<SchemaRecord>) {
    match td {
        TypeDefinition::Object(o) => {
            out.push(type_record(
                &o.name,
                Kind::Object,
                &o.description,
                &o.directives,
            ));
            for f in &o.fields {
                out.push(field_record(&o.name, f, roots));
            }
        }
        TypeDefinition::Interface(i) => {
            out.push(type_record(
                &i.name,
                Kind::Interface,
                &i.description,
                &i.directives,
            ));
            for f in &i.fields {
                out.push(field_record(&i.name, f, roots));
            }
        }
        TypeDefinition::InputObject(io) => {
            out.push(type_record(
                &io.name,
                Kind::InputObject,
                &io.description,
                &io.directives,
            ));
            for f in &io.fields {
                out.push(input_field_record(&io.name, f));
            }
        }
        TypeDefinition::Enum(e) => {
            out.push(type_record(
                &e.name,
                Kind::Enum,
                &e.description,
                &e.directives,
            ));
            for v in &e.values {
                out.push(enum_value_record(&e.name, v));
            }
        }
        TypeDefinition::Union(u) => {
            out.push(type_record(
                &u.name,
                Kind::Union,
                &u.description,
                &u.directives,
            ));
        }
        TypeDefinition::Scalar(s) => {
            out.push(type_record(
                &s.name,
                Kind::Scalar,
                &s.description,
                &s.directives,
            ));
        }
    }
}

fn type_record(
    name: &str,
    kind: Kind,
    description: &Option<String>,
    directives: &[Directive<'_, String>],
) -> SchemaRecord {
    SchemaRecord {
        path: name.to_string(),
        name: name.to_string(),
        kind,
        parent: None,
        type_ref: None,
        args: Vec::new(),
        description: description.clone(),
        deprecated: None,
        directives: directive_names(directives),
    }
}

fn field_record(type_name: &str, f: &Field<'_, String>, roots: &Roots) -> SchemaRecord {
    let kind = roots.field_kind(type_name);
    SchemaRecord {
        path: format!("{}.{}", type_name, f.name),
        name: f.name.clone(),
        kind,
        parent: Some(type_name.to_string()),
        type_ref: Some(type_to_string(&f.field_type)),
        args: f.arguments.iter().map(fmt_input).collect(),
        description: f.description.clone(),
        deprecated: deprecated_reason(&f.directives),
        directives: directive_names(&f.directives),
    }
}

/// Fields/values added by an `extend type ...` block. The base type is defined
/// elsewhere (or upstream in a federated graph), so we index only the added
/// members — routine in schema stitching, so silently dropping them would lose
/// real graph on exactly the large schemas gqls targets.
fn emit_type_extension(te: &TypeExtension<'_, String>, roots: &Roots, out: &mut Vec<SchemaRecord>) {
    match te {
        TypeExtension::Object(o) => {
            for f in &o.fields {
                out.push(field_record(&o.name, f, roots));
            }
        }
        TypeExtension::Interface(i) => {
            for f in &i.fields {
                out.push(field_record(&i.name, f, roots));
            }
        }
        TypeExtension::InputObject(io) => {
            for f in &io.fields {
                out.push(input_field_record(&io.name, f));
            }
        }
        TypeExtension::Enum(e) => {
            for v in &e.values {
                out.push(enum_value_record(&e.name, v));
            }
        }
        // Scalar/Union extensions add only directives/members — no searchable leaf.
        _ => {}
    }
}

fn input_field_record(type_name: &str, f: &InputValue<'_, String>) -> SchemaRecord {
    SchemaRecord {
        path: format!("{type_name}.{}", f.name),
        name: f.name.clone(),
        kind: Kind::InputField,
        parent: Some(type_name.to_string()),
        type_ref: Some(type_to_string(&f.value_type)),
        args: Vec::new(),
        description: f.description.clone(),
        deprecated: deprecated_reason(&f.directives),
        directives: directive_names(&f.directives),
    }
}

fn enum_value_record(type_name: &str, v: &EnumValue<'_, String>) -> SchemaRecord {
    SchemaRecord {
        path: format!("{type_name}.{}", v.name),
        name: v.name.clone(),
        kind: Kind::EnumValue,
        parent: Some(type_name.to_string()),
        type_ref: None,
        args: Vec::new(),
        description: v.description.clone(),
        deprecated: deprecated_reason(&v.directives),
        directives: directive_names(&v.directives),
    }
}

/// `name: Type`, plus ` = default` when the schema gives one — the default is
/// what tells a caller the argument is optional even where the type is
/// non-null, so it belongs in the rendered signature.
fn fmt_input(iv: &InputValue<'_, String>) -> String {
    let base = format!("{}: {}", iv.name, type_to_string(&iv.value_type));
    match &iv.default_value {
        Some(default) => format!("{base} = {default}"),
        None => base,
    }
}

fn type_to_string(t: &Type<'_, String>) -> String {
    match t {
        Type::NamedType(n) => n.clone(),
        Type::ListType(inner) => format!("[{}]", type_to_string(inner)),
        Type::NonNullType(inner) => format!("{}!", type_to_string(inner)),
    }
}

fn directive_names(ds: &[Directive<'_, String>]) -> Vec<String> {
    ds.iter().map(|d| format!("@{}", d.name)).collect()
}

fn deprecated_reason(ds: &[Directive<'_, String>]) -> Option<String> {
    let d = ds.iter().find(|d| d.name == "deprecated")?;
    for (k, v) in &d.arguments {
        if k == "reason" {
            if let Value::String(s) = v {
                return Some(s.clone());
            }
        }
    }
    Some("deprecated".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_federation_v2_schema_extension() {
        let sdl = "extend schema\n  \
            @link(url: \"https://specs.apollo.dev/federation/v2.3\", import: [\"@key\", \"@shareable\"])\n\n\
            type User @key(fields: \"id\") { id: ID! name: String! @shareable }\n\
            type Query { me: User }\n";
        let recs = from_sdl(sdl).expect("federation subgraph should parse");
        assert!(recs.iter().any(|r| r.path == "User.name"));
        assert!(recs
            .iter()
            .any(|r| r.path == "Query.me" && r.kind == Kind::Query));
        assert!(!recs.iter().any(|r| r.name == "link")); // the @link block yields nothing
    }

    #[test]
    fn plain_schema_and_extend_type_are_untouched() {
        let sdl = "schema { query: Q }\n\
            type Q { a: Int }\n\
            type User { id: ID! }\n\
            extend type User { name: String! }\n";
        let recs = from_sdl(sdl).unwrap();
        assert!(recs.iter().any(|r| r.path == "User.name")); // from `extend type`
        assert!(recs.iter().any(|r| r.path == "Q.a"));
    }
}
