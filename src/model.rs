//! The language-agnostic record model: every type, field, arg, enum value,
//! and directive in a schema flattens to one [`SchemaRecord`]. Search and
//! output operate only on these — they never touch a parser or a transport.

use std::str::FromStr;

use serde::Serialize;

/// What a [`SchemaRecord`] describes. Root operation fields get their own
/// kinds (`Query`/`Mutation`/`Subscription`) so ranking can float them to
/// the top the way `rq` floats top-level definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
    Scalar,
    Directive,
    Field,
    InputField,
    EnumValue,
    Query,
    Mutation,
    Subscription,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Object => "object",
            Kind::Interface => "interface",
            Kind::Union => "union",
            Kind::Enum => "enum",
            Kind::InputObject => "input_object",
            Kind::Scalar => "scalar",
            Kind::Directive => "directive",
            Kind::Field => "field",
            Kind::InputField => "input_field",
            Kind::EnumValue => "enum_value",
            Kind::Query => "query",
            Kind::Mutation => "mutation",
            Kind::Subscription => "subscription",
        }
    }

    /// Static ranking bias by kind — roots and named types outrank leaf
    /// args. (In `rq` this is the `kind`-weight table in `score.rs`.)
    pub fn weight(self) -> i64 {
        match self {
            Kind::Query | Kind::Mutation | Kind::Subscription => 60,
            Kind::Object
            | Kind::Interface
            | Kind::Union
            | Kind::Enum
            | Kind::InputObject
            | Kind::Scalar => 40,
            Kind::Directive => 30,
            Kind::Field | Kind::InputField => 20,
            Kind::EnumValue => 10,
        }
    }
}

impl FromStr for Kind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "object" | "objects" | "type" | "types" => Kind::Object,
            "interface" | "interfaces" => Kind::Interface,
            "union" | "unions" => Kind::Union,
            "enum" | "enums" => Kind::Enum,
            "input" | "input_object" | "input_objects" | "inputobject" => Kind::InputObject,
            "scalar" | "scalars" => Kind::Scalar,
            "directive" | "directives" => Kind::Directive,
            "field" | "fields" => Kind::Field,
            "input_field" | "input_fields" | "inputfield" => Kind::InputField,
            "enum_value" | "enum_values" | "enumvalue" => Kind::EnumValue,
            "query" | "queries" => Kind::Query,
            "mutation" | "mutations" => Kind::Mutation,
            "subscription" | "subscriptions" => Kind::Subscription,
            other => anyhow::bail!(
                "unknown kind {other:?} — valid: object, interface, union, enum, \
                 input_object, scalar, directive, field, input_field, enum_value, \
                 query, mutation, subscription (plurals ok)"
            ),
        })
    }
}

/// One searchable schema entity.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaRecord {
    /// Fully-qualified path, e.g. `User.email`, `Query.user`, `@deprecated`.
    pub path: String,
    /// The leaf name matched against — `email`, `user`, `User`.
    pub name: String,
    pub kind: Kind,
    /// Enclosing type name for fields/args/enum values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Return/field/arg type, rendered like `[User!]!`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_ref: Option<String>,
    /// Argument signatures for a field, e.g. `["id: ID!", "first: Int"]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Deprecation reason, if the entity is `@deprecated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Applied directives, rendered like `@auth`. Populated from SDL; always
    /// empty when the schema is loaded by introspection, since applied
    /// directives aren't exposed by the standard introspection query — so the
    /// same schema can differ in this field between its SDL and its endpoint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
}

impl SchemaRecord {
    /// The return/field type with GraphQL's list and non-null wrappers peeled
    /// off — `[User!]!` → `User` — or `None` for a record that has no type
    /// (a type definition itself, a directive). This is the name to compare
    /// against when asking "what returns a `User`?".
    pub fn base_type(&self) -> Option<&str> {
        let base = self
            .type_ref
            .as_deref()?
            .trim_matches(|c| matches!(c, '[' | ']' | '!' | ' '));
        (!base.is_empty()).then_some(base)
    }
}

/// The root operation type names (Query / Mutation / Subscription), used to
/// classify a type's fields as root operations vs. plain object fields. Shared
/// by the SDL and introspection loaders so that one rule lives in one place;
/// each loader fills in whichever roots its source declares.
#[derive(Default)]
pub struct Roots {
    pub query: Option<String>,
    pub mutation: Option<String>,
    pub subscription: Option<String>,
}

impl Roots {
    /// The [`Kind`] for a field defined on `type_name`: a root-operation kind if
    /// `type_name` is a root type, otherwise a plain object [`Kind::Field`].
    pub fn field_kind(&self, type_name: &str) -> Kind {
        if self.query.as_deref() == Some(type_name) {
            Kind::Query
        } else if self.mutation.as_deref() == Some(type_name) {
            Kind::Mutation
        } else if self.subscription.as_deref() == Some(type_name) {
            Kind::Subscription
        } else {
            Kind::Field
        }
    }
}
