//! Draft a ready-to-paste example operation for a matched field.
//!
//! Everything here is mechanical — the schema already says what the arguments
//! are, what the field returns, and which of that type's fields are leaves. The
//! rules, and why each one:
//!
//! * **Arguments you must supply become variables.** Never inline a literal
//!   into the query body; a pasted operation should be parameterized from the
//!   start. An argument the server can fill in for itself — nullable, or with
//!   a schema default — is left out of the operation entirely and listed
//!   underneath, so the query runs as-is and the knobs are still discoverable.
//! * **A placeholder names its type.** `"<ID!>"` says both what to put there
//!   and that it's required; `""` or `0` look like real values and get pasted
//!   by accident.
//! * **One level of selection, leaves only.** A scalar or enum return needs no
//!   selection set at all. An object return gets its scalar/enum fields, and a
//!   `# add fields you need` marker for the object-valued ones — guessing how
//!   deep someone wants to go is worse than leaving a hole.
//! * **An `errors` block only when the schema has one.** The payload/errors
//!   convention is widespread but not universal, so it's expanded only when
//!   that field really exists.
//! * **A nested field is reached through a root.** `Company.employee` isn't
//!   callable on its own, so it's wrapped in a root field that returns
//!   `Company`. When several roots qualify, the caller is told, rather than the
//!   pick being passed off as obvious.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde_json::{Map, Value};

use crate::model::{Kind, SchemaRecord};

/// A drafted operation and the variables it expects.
#[derive(Debug)]
pub struct Example {
    /// The GraphQL document, ready to paste.
    pub operation: String,
    /// A JSON object of placeholder variable values, one per required argument.
    pub variables: Value,
    /// Arguments left out because the server can supply them — rendered as
    /// `field(name: Type = default)`, ready to paste back in.
    pub optional: Vec<String>,
    /// The root field a nested target was reached through, if it needed one.
    pub via: Option<String>,
    /// Other root fields that could have reached a nested target. Non-empty
    /// only when the choice was ambiguous.
    pub alternatives: Vec<String>,
}

/// Draft an operation that reaches `target`.
pub fn build(target: &SchemaRecord, records: &[SchemaRecord]) -> Result<Example> {
    let schema = Schema::index(records);

    // The chain of fields to nest, outermost first. A root operation field is
    // already reachable; anything else needs a root that returns its parent.
    let (chain, via, alternatives) = match target.kind {
        Kind::Query | Kind::Mutation | Kind::Subscription => (vec![target], None, Vec::new()),
        Kind::Field => {
            let parent = target
                .parent
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("{} has no enclosing type", target.path))?;
            let mut roots = schema.roots_returning(parent);
            if roots.is_empty() {
                bail!(
                    "{} isn't reachable in one hop — no root field returns {parent}. \
                     Try `gqls --returns {parent}` to see what's close.",
                    target.path
                );
            }
            let chosen = roots.remove(0);
            let via = Some(chosen.path.clone());
            let alternatives = roots.iter().map(|r| r.path.clone()).collect();
            (vec![chosen, target], via, alternatives)
        }
        other => bail!(
            "can't draft an operation for a {} — pick a field, query, or mutation",
            other.as_str()
        ),
    };

    let operation_kind = match chain[0].kind {
        Kind::Mutation => "mutation",
        Kind::Subscription => "subscription",
        _ => "query",
    };

    // Variables first: every argument along the chain, deduped so a repeated
    // name (two `id`s) doesn't collide in the signature.
    let (vars, optional) = Variables::collect(&chain);

    // Then the selection, innermost outward.
    let leaf_type = chain
        .last()
        .and_then(|r| r.base_type())
        .unwrap_or_default()
        .to_string();
    let mut body = schema.selection(&leaf_type);

    for (depth, field) in chain.iter().enumerate().rev() {
        let args = vars.rendered_for(depth);
        body = if body.is_empty() {
            // A leaf-returning field takes no selection set at all.
            vec![format!("{}{}", field.name, args)]
        } else {
            let mut wrapped = vec![format!("{}{} {{", field.name, args)];
            wrapped.extend(body.into_iter().map(|l| format!("  {l}")));
            wrapped.push("}".to_string());
            wrapped
        };
    }

    let mut operation = String::new();
    operation.push_str(operation_kind);
    operation.push(' ');
    operation.push_str(&pascal_case(&chain.last().unwrap().name));
    operation.push_str(&vars.signature());
    operation.push_str(" {\n");
    for line in &body {
        operation.push_str("  ");
        operation.push_str(line);
        operation.push('\n');
    }
    operation.push_str("}\n");

    Ok(Example {
        operation,
        variables: vars.placeholders(),
        optional,
        via,
        alternatives,
    })
}

/// Records indexed the two ways drafting needs: what kind a type name is, and
/// what fields a type has.
struct Schema<'a> {
    kinds: HashMap<&'a str, Kind>,
    fields: HashMap<&'a str, Vec<&'a SchemaRecord>>,
    roots: Vec<&'a SchemaRecord>,
}

impl<'a> Schema<'a> {
    fn index(records: &'a [SchemaRecord]) -> Self {
        let mut kinds = HashMap::new();
        let mut fields: HashMap<&str, Vec<&SchemaRecord>> = HashMap::new();
        let mut roots = Vec::new();
        for r in records {
            match r.kind {
                Kind::Query | Kind::Mutation | Kind::Subscription => {
                    roots.push(r);
                    if let Some(p) = r.parent.as_deref() {
                        fields.entry(p).or_default().push(r);
                    }
                }
                Kind::Field | Kind::InputField | Kind::EnumValue => {
                    if let Some(p) = r.parent.as_deref() {
                        fields.entry(p).or_default().push(r);
                    }
                }
                _ => {
                    kinds.insert(r.name.as_str(), r.kind);
                }
            }
        }
        Self {
            kinds,
            fields,
            roots,
        }
    }

    /// Root operation fields returning `type_name`, best first. Fewest required
    /// arguments wins: `viewer` is a friendlier entry point than `node(id:)`,
    /// which needs one you may not have yet.
    fn roots_returning(&self, type_name: &str) -> Vec<&'a SchemaRecord> {
        let mut hits: Vec<&SchemaRecord> = self
            .roots
            .iter()
            .copied()
            .filter(|r| {
                r.base_type()
                    .is_some_and(|t| t.eq_ignore_ascii_case(type_name))
            })
            .collect();
        hits.sort_by_key(|r| (required_args(r), r.path.len(), r.path.clone()));
        hits
    }

    /// Whether a type needs no selection set — a scalar, an enum, or a name
    /// the schema never defines (the built-in scalars, which SDL omits).
    fn is_leaf(&self, type_name: &str) -> bool {
        !matches!(
            self.kinds.get(type_name),
            Some(Kind::Object | Kind::Interface | Kind::Union | Kind::InputObject)
        )
    }

    /// The selection set for `type_name`: its leaf fields, plus a marker for
    /// each object-valued field so the hole is visible. Empty for a leaf type.
    fn selection(&self, type_name: &str) -> Vec<String> {
        if type_name.is_empty() || self.is_leaf(type_name) {
            return Vec::new();
        }
        let mut lines = Vec::new();
        let mut deferred = Vec::new();
        for f in self.fields.get(type_name).into_iter().flatten() {
            if f.kind != Kind::Field {
                continue;
            }
            let Some(base) = f.base_type() else { continue };
            if self.is_leaf(base) {
                // A field with required arguments can't be selected bare.
                if f.args.iter().any(|a| a.trim_end().ends_with('!')) {
                    deferred.push(format!("# {}: {} — needs arguments", f.name, base));
                } else {
                    lines.push(f.name.clone());
                }
            } else if f.name.eq_ignore_ascii_case("errors") {
                // The payload/errors convention, expanded only because this
                // schema really has the field.
                let inner = self.selection(base);
                lines.push(format!("{} {{", f.name));
                lines.extend(inner.into_iter().map(|l| format!("  {l}")));
                lines.push("}".to_string());
            } else {
                deferred.push(format!("# {}: {} — add fields you need", f.name, base));
            }
        }
        lines.extend(deferred);
        if lines.is_empty() {
            // A union (no fields of its own), or a type this schema doesn't
            // detail. `__typename` is always valid and keeps the query runnable.
            lines.push("__typename".to_string());
            lines.push("# add inline fragments: ... on ConcreteType { … }".to_string());
        }
        lines
    }
}

/// The operation's variables: one per argument along the field chain.
struct Variables {
    /// `(depth, arg name, variable name, type)`
    entries: Vec<(usize, String, String, String)>,
}

impl Variables {
    /// Split the chain's arguments: the ones a caller must supply become
    /// variables, the rest are returned as notes.
    fn collect(chain: &[&SchemaRecord]) -> (Self, Vec<String>) {
        let mut entries: Vec<(usize, String, String, String)> = Vec::new();
        let mut optional = Vec::new();
        for (depth, field) in chain.iter().enumerate() {
            for arg in &field.args {
                let Arg {
                    name,
                    type_ref,
                    default,
                } = split_arg(arg);
                // A default means the server fills it in, so even a non-null
                // argument needs nothing from the caller.
                if !type_ref.ends_with('!') || default.is_some() {
                    optional.push(format!("{}({})", field.name, arg.trim()));
                    continue;
                }
                // Disambiguate a name already taken by an outer field's arg.
                let taken = entries.iter().any(|(_, _, var, _)| var == name);
                let var = if taken {
                    format!("{}{}", field.name, pascal_case(name))
                } else {
                    name.to_string()
                };
                entries.push((depth, name.to_string(), var, type_ref.to_string()));
            }
        }
        (Self { entries }, optional)
    }

    /// `($id: ID!, $first: Int)`, or empty when there are no arguments.
    fn signature(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let inner: Vec<String> = self
            .entries
            .iter()
            .map(|(_, _, var, ty)| format!("${var}: {ty}"))
            .collect();
        format!("({})", inner.join(", "))
    }

    /// `(id: $id, first: $first)` for the field at `depth`, or empty.
    fn rendered_for(&self, depth: usize) -> String {
        let inner: Vec<String> = self
            .entries
            .iter()
            .filter(|(d, _, _, _)| *d == depth)
            .map(|(_, name, var, _)| format!("{name}: ${var}"))
            .collect();
        if inner.is_empty() {
            String::new()
        } else {
            format!("({})", inner.join(", "))
        }
    }

    /// Every variable is required by construction, so each placeholder names
    /// its type — unmistakably a blank to fill rather than a usable value.
    fn placeholders(&self) -> Value {
        let mut map = Map::new();
        for (_, _, var, ty) in &self.entries {
            map.insert(var.clone(), Value::String(format!("<{ty}>")));
        }
        Value::Object(map)
    }
}

/// How many of a field's arguments are non-null, and so must be supplied.
fn required_args(r: &SchemaRecord) -> usize {
    r.args
        .iter()
        .map(|a| split_arg(a))
        .filter(|a| a.type_ref.ends_with('!') && a.default.is_none())
        .count()
}

/// One parsed argument signature.
struct Arg<'a> {
    name: &'a str,
    type_ref: &'a str,
    default: Option<&'a str>,
}

/// `"first: Int = 10"` → name `first`, type `Int`, default `10`. gqls renders
/// arguments as `name: Type` with ` = default` appended when the schema has one.
fn split_arg(arg: &str) -> Arg<'_> {
    let (name, rest) = arg.split_once(':').unwrap_or((arg, ""));
    let (type_ref, default) = match rest.split_once('=') {
        Some((t, d)) => (t, Some(d.trim())),
        None => (rest, None),
    };
    Arg {
        name: name.trim(),
        type_ref: type_ref.trim(),
        default,
    }
}

/// `updateEmployee` → `UpdateEmployee`, for the operation name.
fn pascal_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        path: &str,
        name: &str,
        kind: Kind,
        parent: Option<&str>,
        type_ref: Option<&str>,
        args: &[&str],
    ) -> SchemaRecord {
        SchemaRecord {
            path: path.into(),
            name: name.into(),
            kind,
            parent: parent.map(Into::into),
            type_ref: type_ref.map(Into::into),
            args: args.iter().map(|a| a.to_string()).collect(),
            description: None,
            deprecated: None,
            directives: vec![],
        }
    }

    /// Query.user(id) -> User { id name role posts(Post) }, plus a mutation
    /// whose payload carries an errors block.
    fn schema() -> Vec<SchemaRecord> {
        vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec("User", "User", Kind::Object, None, None, &[]),
            rec("Post", "Post", Kind::Object, None, None, &[]),
            rec("Role", "Role", Kind::Enum, None, None, &[]),
            rec("Payload", "Payload", Kind::Object, None, None, &[]),
            rec("UserError", "UserError", Kind::Object, None, None, &[]),
            rec("Input", "Input", Kind::InputObject, None, None, &[]),
            rec(
                "Query.user",
                "user",
                Kind::Query,
                Some("Query"),
                Some("User"),
                &["id: ID!"],
            ),
            rec(
                "Query.count",
                "count",
                Kind::Query,
                Some("Query"),
                Some("Int!"),
                &[],
            ),
            rec("User.id", "id", Kind::Field, Some("User"), Some("ID!"), &[]),
            rec(
                "User.name",
                "name",
                Kind::Field,
                Some("User"),
                Some("String"),
                &[],
            ),
            rec(
                "User.role",
                "role",
                Kind::Field,
                Some("User"),
                Some("Role!"),
                &[],
            ),
            rec(
                "User.posts",
                "posts",
                Kind::Field,
                Some("User"),
                Some("[Post!]!"),
                &[],
            ),
            rec(
                "User.avatar",
                "avatar",
                Kind::Field,
                Some("User"),
                Some("String"),
                &["size: Int!"],
            ),
            rec(
                "Mutation.save",
                "save",
                Kind::Mutation,
                Some("Mutation"),
                Some("Payload!"),
                &["input: Input!", "dryRun: Boolean"],
            ),
            rec(
                "Payload.ok",
                "ok",
                Kind::Field,
                Some("Payload"),
                Some("Boolean!"),
                &[],
            ),
            rec(
                "Payload.errors",
                "errors",
                Kind::Field,
                Some("Payload"),
                Some("[UserError!]!"),
                &[],
            ),
            rec(
                "UserError.message",
                "message",
                Kind::Field,
                Some("UserError"),
                Some("String!"),
                &[],
            ),
            rec(
                "Role.ADMIN",
                "ADMIN",
                Kind::EnumValue,
                Some("Role"),
                None,
                &[],
            ),
        ]
    }

    fn build_for(path: &str) -> Example {
        let records = schema();
        let target = records.iter().find(|r| r.path == path).unwrap();
        build(target, &records).unwrap()
    }

    #[test]
    fn root_field_becomes_a_parameterized_query() {
        let ex = build_for("Query.user");
        assert_eq!(
            ex.operation,
            "query User($id: ID!) {\n  \
               user(id: $id) {\n    \
                 id\n    \
                 name\n    \
                 role\n    \
                 # posts: Post — add fields you need\n    \
                 # avatar: String — needs arguments\n  \
               }\n\
             }\n"
        );
        assert_eq!(ex.variables, serde_json::json!({ "id": "<ID!>" }));
    }

    #[test]
    fn a_scalar_return_gets_no_selection_set() {
        let ex = build_for("Query.count");
        assert_eq!(ex.operation, "query Count {\n  count\n}\n");
        assert_eq!(ex.variables, serde_json::json!({}));
    }

    #[test]
    fn mutation_expands_a_real_errors_block() {
        let ex = build_for("Mutation.save");
        assert!(
            ex.operation
                .starts_with("mutation Save($input: Input!) {\n  save(input: $input) {"),
            "{}",
            ex.operation
        );
        // the nullable arg is left out of the operation, but still surfaced
        assert_eq!(ex.optional, ["save(dryRun: Boolean)"]);
        assert!(
            ex.operation.contains("errors {\n      message\n    }"),
            "{}",
            ex.operation
        );
        // only what the caller must supply, typed so it can't be mistaken
        // for a usable value
        assert_eq!(ex.variables, serde_json::json!({ "input": "<Input!>" }));
    }

    #[test]
    fn nested_field_is_wrapped_in_a_root_that_returns_its_type() {
        let ex = build_for("User.posts");
        // User.posts isn't callable directly; Query.user returns a User
        assert_eq!(
            ex.operation,
            "query Posts($id: ID!) {\n  \
               user(id: $id) {\n    \
                 posts {\n      \
                   __typename\n      \
                   # add inline fragments: ... on ConcreteType { … }\n    \
                 }\n  \
               }\n\
             }\n"
        );
    }

    #[test]
    fn an_unreachable_field_is_an_error_not_a_guess() {
        let mut records = schema();
        // nothing returns UserError, so UserError.message can't be reached
        let target = records
            .iter()
            .position(|r| r.path == "UserError.message")
            .unwrap();
        let target = records.remove(target);
        let err = build(&target, &records).unwrap_err().to_string();
        assert!(err.contains("no root field returns UserError"), "{err}");
    }

    #[test]
    fn ambiguous_roots_are_reported_rather_than_hidden() {
        let mut records = schema();
        records.push(rec(
            "Query.viewer",
            "viewer",
            Kind::Query,
            Some("Query"),
            Some("User"),
            &[],
        ));
        let target = records.iter().find(|r| r.path == "User.name").unwrap();
        let ex = build(target, &records).unwrap();
        // both Query.user and Query.viewer return User
        assert_eq!(ex.alternatives.len(), 1);
    }

    #[test]
    fn a_defaulted_argument_is_omitted_even_when_non_null() {
        let records = vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec(
                "Query.feed",
                "feed",
                Kind::Query,
                Some("Query"),
                Some("Int!"),
                // non-null, but the schema supplies a default — nothing is
                // required of the caller
                &["first: Int! = 10", "after: String"],
            ),
        ];
        let target = records.iter().find(|r| r.path == "Query.feed").unwrap();
        let ex = build(target, &records).unwrap();
        assert_eq!(ex.operation, "query Feed {\n  feed\n}\n");
        assert_eq!(ex.variables, serde_json::json!({}));
        assert_eq!(
            ex.optional,
            ["feed(first: Int! = 10)", "feed(after: String)"]
        );
    }

    /// The point of the whole module: what it prints must parse as GraphQL.
    #[test]
    fn every_drafted_operation_is_valid_graphql() {
        for path in [
            "Query.user",
            "Query.count",
            "Mutation.save",
            "User.posts",
            "User.name",
        ] {
            let ex = build_for(path);
            graphql_parser::parse_query::<String>(&ex.operation).unwrap_or_else(|e| {
                panic!("{path} drafted invalid GraphQL: {e}\n{}", ex.operation)
            });
        }
    }

    #[test]
    fn colliding_argument_names_are_disambiguated() {
        let records = vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec("Role", "Role", Kind::Enum, None, None, &[]),
            rec(
                "Role.ADMIN",
                "ADMIN",
                Kind::EnumValue,
                Some("Role"),
                None,
                &[],
            ),
            rec("Thing", "Thing", Kind::Object, None, None, &[]),
            rec(
                "Query.thing",
                "thing",
                Kind::Query,
                Some("Query"),
                Some("Thing"),
                &["id: ID!"],
            ),
            rec(
                "Thing.child",
                "child",
                Kind::Field,
                Some("Thing"),
                Some("String"),
                &["id: ID!", "role: Role!"],
            ),
        ];
        let target = records.iter().find(|r| r.path == "Thing.child").unwrap();
        let ex = build(target, &records).unwrap();
        // the inner `id` collides with the root's, so it's prefixed
        assert!(
            ex.operation.contains("child(id: $childId, role: $role)"),
            "{}",
            ex.operation
        );
        assert_eq!(
            ex.variables,
            serde_json::json!({ "id": "<ID!>", "childId": "<ID!>", "role": "<Role!>" })
        );
    }
}
