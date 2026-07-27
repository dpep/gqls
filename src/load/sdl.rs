//! Parse GraphQL SDL and flatten every definition into [`SchemaRecord`]s.

use anyhow::{anyhow, Result};
use graphql_parser::schema::{
    parse_schema, Definition, Directive, EnumValue, Field, InputValue, Type, TypeDefinition,
    TypeExtension, Value,
};

use crate::model::{Kind, SchemaRecord};

/// Names of the root operation types (overridable via a `schema { ... }` block).
struct Roots {
    query: String,
    mutation: String,
    subscription: String,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            query: "Query".into(),
            mutation: "Mutation".into(),
            subscription: "Subscription".into(),
        }
    }
}

pub fn from_sdl(text: &str) -> Result<Vec<SchemaRecord>> {
    let doc = parse_schema::<String>(text).map_err(|e| anyhow!("parsing SDL: {e}"))?;

    let mut roots = Roots::default();
    for def in &doc.definitions {
        if let Definition::SchemaDefinition(s) = def {
            if let Some(q) = &s.query {
                roots.query = q.clone();
            }
            if let Some(m) = &s.mutation {
                roots.mutation = m.clone();
            }
            if let Some(sub) = &s.subscription {
                roots.subscription = sub.clone();
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
            out.push(type_record(&o.name, Kind::Object, &o.description, &o.directives));
            for f in &o.fields {
                out.push(field_record(&o.name, f, roots));
            }
        }
        TypeDefinition::Interface(i) => {
            out.push(type_record(&i.name, Kind::Interface, &i.description, &i.directives));
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
            out.push(type_record(&e.name, Kind::Enum, &e.description, &e.directives));
            for v in &e.values {
                out.push(enum_value_record(&e.name, v));
            }
        }
        TypeDefinition::Union(u) => {
            out.push(type_record(&u.name, Kind::Union, &u.description, &u.directives));
        }
        TypeDefinition::Scalar(s) => {
            out.push(type_record(&s.name, Kind::Scalar, &s.description, &s.directives));
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
    let kind = if type_name == roots.query {
        Kind::Query
    } else if type_name == roots.mutation {
        Kind::Mutation
    } else if type_name == roots.subscription {
        Kind::Subscription
    } else {
        Kind::Field
    };
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

fn fmt_input(iv: &InputValue<'_, String>) -> String {
    format!("{}: {}", iv.name, type_to_string(&iv.value_type))
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
