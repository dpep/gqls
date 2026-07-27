//! Field → resolver jump.
//!
//! gqls owns the schema; `rq` owns the code. Given a schema field (or type),
//! we encode graphql-ruby's naming conventions as a prioritized list of `rq`
//! queries, run `rq --json` for each, and merge the hits — so "where does
//! `Query.user` resolve?" lands on `Resolvers::User` / `def user` in the server
//! codebase. Naming varies across apps, so we try several conventions and rank
//! by (convention priority, then rq's own confidence) rather than guess one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Kind, SchemaRecord};

/// One code definition rq found, plus the candidate query that surfaced it.
#[derive(Debug, Deserialize, Serialize)]
pub struct RqHit {
    pub name: String,
    pub file: String,
    pub line: u64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub confidence: f64,
    /// The graphql-ruby candidate query that surfaced this hit — set by gqls,
    /// not read from rq, but included in serialized output.
    #[serde(default, skip_deserializing)]
    pub via: String,
    /// Shared leading path components between the schema's package and this
    /// hit's file — the primary rank key when a file schema is known, so a
    /// resolver in the schema's own subgraph outranks a same-named one
    /// elsewhere in the monorepo.
    #[serde(default, skip_deserializing)]
    pub proximity: usize,
    /// Index of the candidate query that surfaced this hit (0 = best).
    #[serde(skip)]
    candidate_rank: usize,
}

/// Resolve `rec` to its code definition(s) via rq, best first. `schema_path`,
/// when a local file, ranks hits by package proximity to the schema — the
/// resolver in the schema's own subgraph wins over a same-named one elsewhere.
pub fn resolve(
    rec: &SchemaRecord,
    code_dir: Option<&str>,
    schema_path: Option<&Path>,
    limit: usize,
) -> Result<Vec<RqHit>> {
    // Collect every candidate's hits first (deduped), then rank as a whole —
    // proximity can only reorder across candidates if we have them all.
    let mut seen = HashSet::new();
    let mut hits: Vec<RqHit> = Vec::new();
    for (idx, cand) in candidates(rec).into_iter().enumerate() {
        for mut hit in run_rq(&cand, code_dir)? {
            if seen.insert(format!("{}:{}", hit.file, hit.line)) {
                hit.via = cand.clone();
                hit.candidate_rank = idx;
                hits.push(hit);
            }
        }
    }

    // Package proximity: shared leading path components between the schema's
    // directory and each hit's file. rq paths are repo-root-relative, so join
    // them onto the code repo's git toplevel to compare absolute paths.
    if let Some(schema_dir) = schema_path
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        if let Some(root) = git_toplevel(code_dir).and_then(|r| std::fs::canonicalize(r).ok()) {
            for h in &mut hits {
                let file_abs = root.join(&h.file);
                let hit_dir = file_abs.parent().unwrap_or(&root);
                h.proximity = shared_prefix(&schema_dir, hit_dir);
            }
        }
    }

    hits.sort_by(|a, b| {
        b.proximity
            .cmp(&a.proximity)
            .then(a.candidate_rank.cmp(&b.candidate_rank))
            .then(b.confidence.total_cmp(&a.confidence))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// The git repo root containing `dir` (or gqls's cwd) — rq's file paths are
/// relative to it.
fn git_toplevel(dir: Option<&str>) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"]);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Count of shared leading path components.
fn shared_prefix(a: &Path, b: &Path) -> usize {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .count()
}

/// graphql-ruby query candidates for a record, highest-priority first. Uses
/// rq's qualified syntax (`Class#method`, `Module::Class`).
pub fn candidates(rec: &SchemaRecord) -> Vec<String> {
    let field = &rec.name;
    let snake = to_snake(field);
    let pascal = to_pascal(field);
    let mut c = Vec::new();

    match rec.kind {
        Kind::Mutation => {
            c.push(format!("Mutations::{pascal}"));
            c.push(pascal.clone());
            c.push(format!("{pascal}Mutation"));
            if let Some(t) = &rec.parent {
                c.push(format!("{t}Type#{snake}"));
            }
            c.push(snake.clone());
        }
        Kind::Query | Kind::Subscription => {
            c.push(format!("Resolvers::{pascal}"));
            c.push(format!("{pascal}Resolver"));
            c.push(format!("Resolvers::{pascal}Resolver"));
            if let Some(t) = &rec.parent {
                c.push(format!("{t}Type#{snake}"));
            }
            c.push(snake.clone());
        }
        Kind::Field | Kind::InputField => {
            if let Some(t) = &rec.parent {
                c.push(format!("{t}Type#{snake}"));
                c.push(format!("{t}#{snake}"));
                c.push(format!("Types::{t}Type#{snake}"));
            }
            c.push(snake.clone());
        }
        // a type: jump to its `XType` / `Types::X` class
        Kind::Object | Kind::Interface | Kind::InputObject | Kind::Union | Kind::Enum
        | Kind::Scalar => {
            let t = &rec.name;
            c.push(format!("{t}Type"));
            c.push(format!("Types::{t}"));
            c.push(format!("Types::{t}Type"));
            c.push(t.clone());
        }
        Kind::EnumValue | Kind::Directive => {
            c.push(pascal.clone());
            c.push(snake.clone());
        }
    }
    c
}

fn run_rq(query: &str, dir: Option<&str>) -> Result<Vec<RqHit>> {
    let mut cmd = Command::new("rq");
    cmd.arg("--json").arg("--limit").arg("5").arg(query);
    // rq searches the *current repo*, so run it from the server code dir (else
    // gqls's own cwd) rather than passing a cross-repo path filter.
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let output = cmd
        .output()
        .map_err(|e| anyhow!("running rq: {e} — is `rq` installed and on PATH?"))?;
    // rq: exit 0 = hits (JSON array), 1 = no match (a `{status}` object) — both
    // are normal. Any other code is a real rq failure (not a repo, bad flag):
    // surface it instead of silently reporting "no resolver found".
    match output.status.code() {
        Some(0) | Some(1) => Ok(serde_json::from_slice(&output.stdout).unwrap_or_default()),
        other => {
            let msg = String::from_utf8_lossy(&output.stderr);
            bail!(
                "rq failed (exit {other:?}) for {query:?}: {}",
                msg.lines().next().unwrap_or("").trim()
            )
        }
    }
}

/// GraphQL `nameWithOwner` → Ruby `name_with_owner`.
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// GraphQL `createUser` → Ruby class stem `CreateUser`.
fn to_pascal(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, parent: Option<&str>, kind: Kind) -> SchemaRecord {
        SchemaRecord {
            path: parent.map_or_else(|| name.into(), |p| format!("{p}.{name}")),
            name: name.into(),
            kind,
            parent: parent.map(String::from),
            type_ref: None,
            args: vec![],
            description: None,
            deprecated: None,
            directives: vec![],
        }
    }

    #[test]
    fn casing() {
        assert_eq!(to_snake("nameWithOwner"), "name_with_owner");
        assert_eq!(to_pascal("createUser"), "CreateUser");
    }

    #[test]
    fn root_query_prefers_resolver_then_type_method() {
        let c = candidates(&rec("user", Some("Query"), Kind::Query));
        assert_eq!(c[0], "Resolvers::User");
        assert!(c.contains(&"QueryType#user".to_string()));
    }

    #[test]
    fn mutation_prefers_mutation_class() {
        let c = candidates(&rec("createUser", Some("Mutation"), Kind::Mutation));
        assert_eq!(c[0], "Mutations::CreateUser");
    }

    #[test]
    fn object_field_targets_the_type_method() {
        let c = candidates(&rec("nameWithOwner", Some("Repository"), Kind::Field));
        assert_eq!(c[0], "RepositoryType#name_with_owner");
    }
}
