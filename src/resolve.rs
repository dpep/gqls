//! Field → resolver jump.
//!
//! gqls owns the schema; `rq` owns the code. Given a schema field (or type),
//! we encode graphql-ruby's naming conventions as a prioritized list of `rq`
//! queries, run `rq --json` for each, and merge the hits — so "where does
//! `Query.user` resolve?" lands on `Resolvers::User` / `def user` in the server
//! codebase. Naming varies across apps, so we try several conventions and rank
//! by (convention priority, then rq's own confidence) rather than guess one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// The enclosing module/class rq reports, used to check that a hit really
    /// sits where the candidate said it would.
    #[serde(default)]
    pub parent: Option<String>,
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
    /// Whether the candidate that found this was a bare name search rather
    /// than a graphql-ruby convention — a guess, and labelled as one.
    #[serde(default, skip_deserializing)]
    pub loose: bool,
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
    // Collect every candidate's hits first, then rank as a whole — proximity
    // can only reorder across candidates once we have them all.
    //
    // The same location often turns up under several candidates. Keep the best
    // account of it rather than the first: a fuzzy `Resolvers::User` may stumble
    // onto the very definition that `Query#user` then names exactly, and
    // recording it as a guess because the weaker query got there first would
    // bury the right answer.
    // The candidates are independent probes of different naming conventions —
    // nothing about `Resolvers::Employee` depends on `Query#employee` — and each
    // pays rq's process spawn and repo detection before it does any work. Run
    // them at once so a lookup costs the slowest candidate rather than the sum,
    // then merge below in the original candidate order, which is the order the
    // ranking is defined against. Under `-v` this interleaves the candidates'
    // rq traces on stderr, since those stream straight through.
    let cands = candidates(rec);
    let found: Vec<Result<Vec<RqHit>>> = {
        use rayon::prelude::*;
        cands
            .par_iter()
            .map(|cand| run_rq(&cand.query, code_dir))
            .collect()
    };

    let mut best: HashMap<String, RqHit> = HashMap::new();
    for (idx, (cand, found)) in cands.iter().zip(found).enumerate() {
        // Reported in candidate order, not completion order, so the first
        // failure is the same one the sequential version would have raised.
        let found = found?;
        crate::detail!("rq candidate {:?} -> {} hit(s)", cand.query, found.len());
        for mut hit in found {
            hit.via = cand.query.clone();
            hit.candidate_rank = idx;
            // A convention only counts if the hit actually sits where the
            // convention said. Otherwise it's a name match wearing a
            // convention's name.
            hit.loose = cand.loose || !satisfies(&cand.query, &hit);
            let key = format!("{}:{}", hit.file, hit.line);
            match best.get(&key) {
                Some(prev) if (prev.loose, prev.candidate_rank) <= (hit.loose, idx) => {}
                _ => {
                    best.insert(key, hit);
                }
            }
        }
    }
    let mut hits: Vec<RqHit> = best.into_values().collect();

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

    // Which convention matched comes first: a hit from `Mutations::VerbNoun`
    // is evidence, while proximity is a tiebreak between equally-good matches.
    // Ranking on proximity first let five unrelated files that happen to sit
    // one directory closer bury a confident match.
    hits.sort_by(|a, b| {
        a.loose
            .cmp(&b.loose)
            .then(a.candidate_rank.cmp(&b.candidate_rank))
            .then(b.proximity.cmp(&a.proximity))
            .then(b.confidence.total_cmp(&a.confidence))
            // a total order, so equally-ranked hits don't shuffle between runs
            .then_with(|| (&a.file, a.line).cmp(&(&b.file, b.line)))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// Whether a hit actually satisfies the qualification the candidate asked for.
///
/// rq is a fuzzy navigator by design: a query for `Resolvers::User` will
/// cheerfully return a bare `class User` in `app/models`, because the name
/// matches even though the namespace doesn't. That's a fine search result and a
/// terrible resolver answer — the whole point of asking for `Resolvers::User`
/// was to test a convention, and a hit outside `Resolvers` didn't pass it. A
/// bare candidate (`user`) qualifies nothing, so there's nothing to check.
fn satisfies(candidate: &str, hit: &RqHit) -> bool {
    let qualifier = match candidate.rsplit_once('#') {
        Some((q, _)) => q,
        None => match candidate.rsplit_once("::") {
            Some((q, _)) => q,
            None => return true,
        },
    };
    let parent = hit.parent.as_deref().unwrap_or_default();
    // Every segment asked for has to appear in the hit's own path — `Query`
    // matches `SomeSubgraph::Schema::Root::Query`, `Resolvers` matches nothing
    // at the top level of app/models.
    qualifier
        .split("::")
        .all(|want| parent.split("::").any(|have| have == want))
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
pub struct Candidate {
    pub query: String,
    /// A bare name search, matching any symbol anywhere in the repo rather than
    /// a graphql-ruby convention. Kept because it's sometimes the only thing
    /// that hits, ranked last, and reported — its hits are guesses.
    pub loose: bool,
}

impl Candidate {
    fn convention(query: String) -> Self {
        Self {
            query,
            loose: false,
        }
    }
    fn fallback(query: String) -> Self {
        Self { query, loose: true }
    }
}

pub fn candidates(rec: &SchemaRecord) -> Vec<Candidate> {
    let field = &rec.name;
    let snake = to_snake(field);
    let pascal = to_pascal(field);
    let mut c = Vec::new();

    match rec.kind {
        Kind::Mutation => {
            c.push(Candidate::convention(format!("Mutations::{pascal}")));
            c.push(Candidate::convention(format!("{pascal}Mutation")));
            if let Some(t) = &rec.parent {
                // `MutationType#field`, and the bare root class name that
                // federated subgraphs use (`…::Root::Mutation#field`).
                c.push(Candidate::convention(format!("{t}Type#{snake}")));
                c.push(Candidate::convention(format!("{t}#{snake}")));
            }
            c.push(Candidate::fallback(pascal.clone()));
            c.push(Candidate::fallback(snake.clone()));
        }
        Kind::Query | Kind::Subscription => {
            c.push(Candidate::convention(format!("Resolvers::{pascal}")));
            c.push(Candidate::convention(format!("{pascal}Resolver")));
            c.push(Candidate::convention(format!(
                "Resolvers::{pascal}Resolver"
            )));
            if let Some(t) = &rec.parent {
                // A federated subgraph names its root class `Query`, not
                // `QueryType`, so try both rather than assuming the literal.
                c.push(Candidate::convention(format!("{t}Type#{snake}")));
                c.push(Candidate::convention(format!("{t}#{snake}")));
            }
            c.push(Candidate::fallback(snake.clone()));
        }
        Kind::Field | Kind::InputField => {
            if let Some(t) = &rec.parent {
                c.push(Candidate::convention(format!("{t}Type#{snake}")));
                c.push(Candidate::convention(format!("{t}#{snake}")));
                c.push(Candidate::convention(format!("Types::{t}Type#{snake}")));
            }
            c.push(Candidate::fallback(snake.clone()));
        }
        // a type: jump to its `XType` / `Types::X` class
        Kind::Object
        | Kind::Interface
        | Kind::InputObject
        | Kind::Union
        | Kind::Enum
        | Kind::Scalar => {
            let t = &rec.name;
            c.push(Candidate::convention(format!("{t}Type")));
            c.push(Candidate::convention(format!("Types::{t}")));
            c.push(Candidate::convention(format!("Types::{t}Type")));
            c.push(Candidate::fallback(t.clone()));
        }
        Kind::EnumValue | Kind::Directive => {
            c.push(Candidate::fallback(pascal.clone()));
            c.push(Candidate::fallback(snake.clone()));
        }
    }
    c
}

fn run_rq(query: &str, dir: Option<&str>) -> Result<Vec<RqHit>> {
    let verbose = crate::logging::is_verbose();
    let mut cmd = Command::new("rq");
    // Ten, not five: the correct definition was being cut inside rq before
    // gqls could rank it — two structurally identical fields differed only in
    // where they fell in one noisy list. The final `limit` still applies.
    cmd.arg("--json").arg("--limit").arg("10");
    // Mirror `gqls -v` into rq: it traces its own decisions (root, coverage,
    // warming) to stderr, which we let stream straight to the terminal. Without
    // -v we keep capturing rq's stderr so a failure can surface its message.
    if verbose {
        cmd.arg("--verbose");
    }
    cmd.arg(query);
    cmd.stdout(Stdio::piped());
    cmd.stderr(if verbose {
        Stdio::inherit()
    } else {
        Stdio::piped()
    });
    // rq searches the *current repo*, so run it from the server code dir (else
    // gqls's own cwd) rather than passing a cross-repo path filter.
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let output = match cmd.spawn() {
        Ok(child) => child
            .wait_with_output()
            .map_err(|e| anyhow!("running rq: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "--resolve needs `rq` (a code-navigation CLI), which isn't installed. Install it:\n  \
             brew install dpep/tools/rq\n  \
             cargo install --git https://github.com/dpep/rq\n\
             Then re-run — see https://github.com/dpep/rq"
        ),
        Err(e) => bail!("running rq: {e}"),
    };
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
            possible_types: vec![],
        }
    }

    #[test]
    fn casing() {
        assert_eq!(to_snake("nameWithOwner"), "name_with_owner");
        assert_eq!(to_pascal("createUser"), "CreateUser");
    }

    /// The queries a record produces, in order.
    fn queries(rec: &SchemaRecord) -> Vec<String> {
        candidates(rec).into_iter().map(|c| c.query).collect()
    }

    #[test]
    fn root_query_prefers_resolver_then_type_method() {
        let c = queries(&rec("user", Some("Query"), Kind::Query));
        assert_eq!(c[0], "Resolvers::User");
        assert!(c.contains(&"QueryType#user".to_string()));
        // a federated subgraph's root class is `Query`, not `QueryType`
        assert!(c.contains(&"Query#user".to_string()), "{c:?}");
    }

    #[test]
    fn mutation_prefers_mutation_class() {
        let c = queries(&rec("createUser", Some("Mutation"), Kind::Mutation));
        assert_eq!(c[0], "Mutations::CreateUser");
    }

    fn hit(parent: Option<&str>) -> RqHit {
        RqHit {
            name: "user".into(),
            file: "f.rb".into(),
            line: 1,
            kind: "method".into(),
            confidence: 0.5,
            parent: parent.map(Into::into),
            via: String::new(),
            loose: false,
            proximity: 0,
            candidate_rank: 0,
        }
    }

    #[test]
    fn a_hit_outside_the_namespace_asked_for_is_not_a_convention_match() {
        // rq is fuzzy: `Resolvers::User` returns a bare `class User` in
        // app/models. The name matches; the convention was not met.
        assert!(!satisfies("Resolvers::User", &hit(None)));
        assert!(satisfies("Resolvers::User", &hit(Some("Resolvers"))));

        // a federated root class satisfies `Query#user` even when deeply nested
        assert!(satisfies(
            "Query#user",
            &hit(Some("SomeSubgraph::Schema::Root::Query"))
        ));
        assert!(!satisfies("Query#user", &hit(Some("Types::UserType"))));

        // a bare candidate qualifies nothing, so there's nothing to verify
        assert!(satisfies("user", &hit(None)));
    }

    #[test]
    fn bare_name_searches_are_marked_loose_and_come_last() {
        let c = candidates(&rec("user", Some("Query"), Kind::Query));
        let first_loose = c.iter().position(|c| c.loose).expect("a fallback exists");
        // every convention is tried before any bare-name guess
        assert!(
            c[..first_loose].iter().all(|c| !c.loose),
            "{:?}",
            c.iter().map(|c| (&c.query, c.loose)).collect::<Vec<_>>()
        );
        assert!(c[first_loose..].iter().all(|c| c.loose));
        assert_eq!(c[first_loose].query, "user");
    }

    #[test]
    fn object_field_targets_the_type_method() {
        let c = queries(&rec("nameWithOwner", Some("Repository"), Kind::Field));
        assert_eq!(c[0], "RepositoryType#name_with_owner");
    }
}
