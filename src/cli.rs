//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::Parser;
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::search;

#[derive(Parser)]
#[command(name = "gqls", version, about = "Fuzzy + semantic search over a GraphQL schema.")]
struct Cli {
    /// Search query. Fuzzy by default; abbreviations like `usr` match `User`,
    /// and `Type.field` queries match against the qualified path.
    query: String,

    /// Schema source: a path to a `.graphql`/`.graphqls` SDL file, or an
    /// http(s) URL (introspection — not implemented yet). If omitted, gqls
    /// searches the current directory tree for a schema.
    source: Option<String>,

    /// Restrict to a kind (object, field, query, mutation, enum, scalar, ...).
    #[arg(short, long)]
    kind: Option<String>,

    /// Maximum number of results.
    #[arg(short, long, default_value_t = 20)]
    limit: usize,

    /// Pretty JSON array.
    #[arg(short, long, conflicts_with = "ndjson")]
    json: bool,

    /// Newline-delimited JSON (one record per line).
    #[arg(short = 'J', long)]
    ndjson: bool,

    /// Semantic (embedding) search instead of fuzzy. Requires a build with
    /// `--features semantic`.
    #[arg(short, long)]
    semantic: bool,

    /// Embedding model for --semantic: a local dir / `.onnx` path, or a
    /// HuggingFace `org/name` id. Defaults to all-MiniLM-L6-v2.
    #[arg(long)]
    model: Option<String>,

    /// Resolve the top match to its graphql-ruby resolver/method in code via
    /// `rq` (must be installed) — find the field, then jump to its definition.
    #[arg(short = 'R', long)]
    resolve: bool,

    /// Directory of the server code for --resolve (defaults to rq's index).
    #[arg(long)]
    code: Option<String>,
}

/// A ranked result — from either the fuzzy scorer or the semantic ranker, so
/// both flow through one output path.
struct Match<'a> {
    record: &'a SchemaRecord,
    score: f64,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    let kind: Option<Kind> = match &cli.kind {
        Some(s) => Some(s.parse()?),
        None => None,
    };

    let source = match cli.source {
        Some(s) => s,
        None => load::discover()?,
    };
    let records = load::load(&source)?;

    if cli.resolve {
        return run_resolve(&cli.query, &records, kind, cli.code.as_deref(), cli.limit);
    }

    let matches: Vec<Match> = if cli.semantic {
        #[cfg(feature = "semantic")]
        {
            crate::semantic::search(&cli.query, &records, kind, cli.limit, cli.model.as_deref())
                .into_iter()
                .map(|(score, record)| Match { record, score })
                .collect()
        }
        #[cfg(not(feature = "semantic"))]
        {
            let _ = &cli.model;
            anyhow::bail!(
                "this build has no semantic search — rebuild with `cargo build --features semantic`"
            );
        }
    } else {
        search::search(&cli.query, &records, kind, cli.limit)
            .into_iter()
            .map(|h| Match {
                record: h.record,
                score: h.score as f64,
            })
            .collect()
    };

    if cli.json {
        print_json(&matches, false)?;
    } else if cli.ndjson {
        print_json(&matches, true)?;
    } else {
        print_text(&matches);
    }
    Ok(())
}

#[derive(Serialize)]
struct Out<'a> {
    #[serde(flatten)]
    record: &'a SchemaRecord,
    score: f64,
}

fn print_json(matches: &[Match], ndjson: bool) -> Result<()> {
    let rows: Vec<Out> = matches
        .iter()
        .map(|m| Out {
            record: m.record,
            score: m.score,
        })
        .collect();

    if ndjson {
        for row in &rows {
            println!("{}", serde_json::to_string(row)?);
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    }
    Ok(())
}

fn print_text(matches: &[Match]) {
    let width = matches
        .iter()
        .map(|m| display_path(m.record).len())
        .max()
        .unwrap_or(0)
        .min(48);

    for m in matches {
        let r = m.record;
        let path = display_path(r);
        let ret = r
            .type_ref
            .as_deref()
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        let dep = if r.deprecated.is_some() {
            " (deprecated)"
        } else {
            ""
        };
        println!("{path:<width$}{ret}  [{kind}]{dep}", kind = r.kind.as_str());
    }
}

/// Fuzzy-find the field, then hand it to rq to locate its resolver in code.
fn run_resolve(
    query: &str,
    records: &[SchemaRecord],
    kind: Option<Kind>,
    code: Option<&str>,
    limit: usize,
) -> Result<()> {
    let Some(top) = search::search(query, records, kind, 1).into_iter().next() else {
        anyhow::bail!("no schema entity matches {query:?} to resolve");
    };
    eprintln!("gqls: resolving {} …", top.record.path);
    let hits = crate::resolve::resolve(top.record, code, limit.min(10))?;
    if hits.is_empty() {
        eprintln!(
            "gqls: no code definition found for {} (tried graphql-ruby rq candidates)",
            top.record.path
        );
    }
    for h in &hits {
        println!("{}:{}  {}  (via {})", h.file, h.line, h.name, h.via);
    }
    Ok(())
}

/// `Query.user(id: ID!, first: Int)` — path plus a compact arg signature.
fn display_path(r: &SchemaRecord) -> String {
    if r.args.is_empty() {
        r.path.clone()
    } else {
        format!("{}({})", r.path, r.args.join(", "))
    }
}
