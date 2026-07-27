//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::search;

const EXAMPLES: &str = "\
EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls User.email                     qualified Type.field query
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
  gqls 'cancel a subscription' -s     semantic search (build --features semantic)
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)
";

#[derive(Parser)]
#[command(
    name = "gqls",
    version,
    about = "Search a GraphQL schema — fuzzy, semantic, or straight to the resolver.",
    long_about = "Find the types, fields, args, and directives in a GraphQL schema from the \
                  terminal. The source is an SDL file, a local introspection JSON dump, or a live \
                  http(s) endpoint; with none given, gqls discovers a schema in the current tree. \
                  Fuzzy by default; --semantic ranks by meaning; --resolve jumps to the \
                  graphql-ruby resolver via rq. All modes support -j/--json and -J/--ndjson.",
    after_help = EXAMPLES
)]
struct Cli {
    /// Search query. Fuzzy by default; abbreviations like `usr` match `User`,
    /// and `Type.field` queries match against the qualified path.
    #[arg(required_unless_present_any = ["clear_cache", "completions"])]
    query: Option<String>,

    /// Schema source: a `.graphql`/`.graphqls` SDL file, a `.json` introspection
    /// dump, or an http(s) URL (introspected live). If omitted, gqls searches
    /// the current directory tree for a schema.
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

    /// Force a re-embed for --semantic, overwriting the cache. Schema edits
    /// already re-embed on their own; use this for changes the cache can't see
    /// (e.g. a new model).
    #[arg(long)]
    refresh: bool,

    /// Delete all cached embedding vector files, then exit.
    #[arg(long)]
    clear_cache: bool,

    /// Print a shell completion script (bash, zsh, fish, ...) to stdout, then exit.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Jump the top match to its graphql-ruby resolver/method in code, via
    /// `rq` (must be installed).
    #[arg(short = 'R', long)]
    resolve: bool,

    /// Directory of the server code for --resolve (defaults to rq's index).
    #[arg(long)]
    code: Option<String>,
}

/// The chosen output format — computed once, honored by every mode.
#[derive(Clone, Copy)]
enum Output {
    Text,
    Json,
    Ndjson,
}

/// A ranked result — from either the fuzzy scorer or the semantic ranker, so
/// both flow through one output path.
struct Match<'a> {
    record: &'a SchemaRecord,
    score: f64,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    if cli.clear_cache {
        #[cfg(feature = "_semantic")]
        {
            let n = crate::semantic::clear_cache();
            eprintln!("gqls: cleared {n} cached vector file(s)");
            return Ok(());
        }
        #[cfg(not(feature = "_semantic"))]
        anyhow::bail!("no embedding cache in this build (built without --features semantic)");
    }

    // clap guarantees this is present (required_unless_present = clear_cache).
    let query = cli
        .query
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("a QUERY is required (see --help)"))?;

    let output = if cli.json {
        Output::Json
    } else if cli.ndjson {
        Output::Ndjson
    } else {
        Output::Text
    };

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
        return run_resolve(query, &records, kind, cli.code.as_deref(), cli.limit, output);
    }

    let matches: Vec<Match> = if cli.semantic {
        #[cfg(feature = "_semantic")]
        {
            crate::semantic::search(query, &records, kind, cli.limit, cli.model.as_deref(), cli.refresh)
                .into_iter()
                .map(|(score, record)| Match { record, score })
                .collect()
        }
        #[cfg(not(feature = "_semantic"))]
        {
            let _ = (&cli.model, cli.refresh);
            anyhow::bail!(
                "this build has no semantic search — rebuild with `cargo build --features semantic`"
            );
        }
    } else {
        search::search(query, &records, kind, cli.limit)
            .into_iter()
            .map(|h| Match {
                record: h.record,
                score: h.score as f64,
            })
            .collect()
    };

    if matches.is_empty() {
        eprintln!("gqls: no matches for {query:?}");
    }
    output.write_matches(&matches)
}

impl Output {
    fn write_matches(self, matches: &[Match]) -> Result<()> {
        #[derive(Serialize)]
        struct Row<'a> {
            #[serde(flatten)]
            record: &'a SchemaRecord,
            score: f64,
        }
        let rows = || {
            matches.iter().map(|m| Row {
                record: m.record,
                score: m.score,
            })
        };
        match self {
            Output::Json => println!("{}", serde_json::to_string_pretty(&rows().collect::<Vec<_>>())?),
            Output::Ndjson => {
                for row in rows() {
                    println!("{}", serde_json::to_string(&row)?);
                }
            }
            Output::Text => print_text(matches),
        }
        Ok(())
    }
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
    output: Output,
) -> Result<()> {
    if code.is_none() {
        eprintln!(
            "gqls: no --code given; resolving against rq's index for the current directory"
        );
    }
    let Some(top) = search::search(query, records, kind, 1).into_iter().next() else {
        anyhow::bail!("no schema entity matches {query:?} to resolve");
    };
    eprintln!("gqls: resolving {} …", top.record.path);
    let hits = crate::resolve::resolve(top.record, code, limit.min(10))?;

    match output {
        Output::Json => println!("{}", serde_json::to_string_pretty(&hits)?),
        Output::Ndjson => {
            for h in &hits {
                println!("{}", serde_json::to_string(h)?);
            }
        }
        Output::Text => {
            if hits.is_empty() {
                eprintln!(
                    "gqls: no code definition found for {} (tried graphql-ruby rq candidates)",
                    top.record.path
                );
            }
            for h in &hits {
                println!("{}:{}  {}  (via {})", h.file, h.line, h.name, h.via);
            }
        }
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
