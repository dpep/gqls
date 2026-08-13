//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::render::{self, Extras, Match};
use crate::search;

/// The semantic-only flags (--semantic, --model, --refresh, --clear-cache) are
/// hidden from --help on builds without the feature, where they'd only error.
const HIDE_SEMANTIC: bool = !cfg!(feature = "_semantic");

/// The help text, assembled per feature configuration from shared pieces.
///
/// Hand-maintained copies drift — a fuzzy-only build was advertising semantic
/// search in `long_about` and disclaiming it in `EXAMPLES`, forty lines apart —
/// and nothing in the test suite reads help text, so drift is silent. Written
/// as macros because `concat!` takes literals, and a macro expanding to one
/// counts.
macro_rules! about_head {
    () => {
        "Find the types, fields, args, and directives in a GraphQL schema from the terminal. \
         The source is an SDL file, a local introspection JSON dump, or a live http(s) \
         endpoint; with none given, gqls discovers a schema in the current tree. "
    };
}

macro_rules! about_tail {
    () => {
        "Name one record and gqls explains it rather than listing: its description in full, \
         deprecation, directives, an abstract type's members, an enum's values, and what \
         references it. --example drafts an operation to paste, --resolve jumps to the \
         graphql-ruby resolver via rq. All modes support -j/--json and -J/--ndjson."
    };
}

macro_rules! example_body {
    () => {
        "EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls Role                           name one record, get it explained in full
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls query user                     ...or lead with the kind word
  gqls User.email                     qualified Type.field query
  gqls User.                          list a type's fields (or 'User.*')
  gqls 'User.{first,last}Name'        also ? for one char, {a,b} to alternate
  gqls --returns Company -k query     find fields by return type, not name
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
"
    };
}

macro_rules! example_tail {
    () => {
        "  gqls Mutation.createUser -e         draft an operation to paste
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)
"
    };
}

#[cfg(feature = "_semantic")]
const ABOUT: &str = "Search a GraphQL schema — fuzzy, semantic, or straight to the resolver.";
#[cfg(not(feature = "_semantic"))]
const ABOUT: &str = "Search a GraphQL schema — fuzzy, or straight to the resolver.";

#[cfg(feature = "_semantic")]
const LONG_ABOUT: &str = concat!(
    about_head!(),
    "Fuzzy and semantic results are ranked together by default (--semantic or --fuzzy forces \
     one). ",
    about_tail!()
);
#[cfg(not(feature = "_semantic"))]
const LONG_ABOUT: &str = concat!(about_head!(), about_tail!());

#[cfg(feature = "_semantic")]
const EXAMPLES: &str = concat!(
    example_body!(),
    "  gqls cancel a subscription          rank by meaning — no quotes needed\n",
    example_tail!()
);

#[cfg(not(feature = "_semantic"))]
const EXAMPLES: &str = concat!(
    example_body!(),
    "  gqls cancel a subscription          several words are one query\n",
    example_tail!(),
    "\nSemantic search (--semantic, rank by meaning) is not compiled into this build. Enable it:
  cargo install gqls-cli --features semantic
  brew install dpep/tools/gqls
"
);

#[derive(Parser)]
#[command(
    name = "gqls",
    version,
    about = ABOUT,
    long_about = LONG_ABOUT,
    after_help = EXAMPLES
)]
struct Cli {
    /// Search query, then optionally the schema source.
    ///
    /// The query is fuzzy by default; abbreviations like `usr` match `User`,
    /// and `Type.field` queries match against the qualified path. A trailing
    /// dot lists a type's fields (`User.`); the general wildcards (`*` any
    /// run, `?` one char, `{a,b}` alternatives) enumerate too, but quote them
    /// so the shell doesn't expand them first.
    /// Omit it entirely when piping queries on stdin, one per line.
    ///
    /// Several words are one query, so `gqls cancel a subscription` needs no
    /// quotes. A leading kind — `gqls query user`, `gqls type User` — filters
    /// by it, the same as `-k`.
    ///
    /// The source is a `.graphql`/`.graphqls` SDL file, a `.json` introspection
    /// dump, or an http(s) URL (introspected live). Recognised wherever it
    /// appears; with none given, gqls searches the current directory tree.
    #[arg(value_name = "QUERY", num_args = 0..)]
    args: Vec<String>,

    /// Restrict to a kind (object, field, query, mutation, enum, scalar, ...).
    #[arg(short, long)]
    kind: Option<String>,

    /// Restrict to fields returning this type, ignoring `[]`/`!` wrappers —
    /// `--returns Company` finds `myEmployer: Company`. Wildcards work
    /// (`--returns '*Payload'`). With no QUERY, everything matching is listed.
    #[arg(long, value_name = "TYPE")]
    returns: Option<String>,

    /// Maximum number of results.
    #[arg(short, long, default_value_t = 20)]
    limit: usize,

    /// Pretty JSON array.
    #[arg(short, long, conflicts_with = "ndjson")]
    json: bool,

    /// Newline-delimited JSON (one record per line).
    #[arg(short = 'J', long)]
    ndjson: bool,

    /// Omit schema descriptions from text output (they're shown by default;
    /// `--json`/`--ndjson` always carry the full text).
    #[arg(short = 'D', long)]
    no_description: bool,

    /// Always list matches, even when the query names exactly one of them.
    #[arg(long)]
    no_explain: bool,

    /// Force semantic-only search. By default fuzzy and semantic results are
    /// combined once the schema's vectors are cached.
    #[arg(long, hide = HIDE_SEMANTIC)]
    semantic: bool,

    /// Force fuzzy-only search — skip the semantic combine.
    #[arg(long, conflicts_with = "semantic", hide = HIDE_SEMANTIC)]
    fuzzy: bool,

    /// Embedding model for --semantic: a local dir / `.onnx` path, or a
    /// HuggingFace `org/name` id. Defaults to all-MiniLM-L6-v2.
    #[arg(long, hide = HIDE_SEMANTIC)]
    model: Option<String>,

    /// Bypass every cache: re-walk for the schema, re-fetch a URL, re-embed.
    /// Schema edits already re-embed on their own; use this for changes a cache
    /// can't see — a new model, or a schema that moved.
    #[arg(long)]
    refresh: bool,

    /// Delete every cached file — introspection responses, parsed records,
    /// discovered schema paths, and embedding vectors — then exit.
    #[arg(long)]
    clear_cache: bool,

    /// Pre-embed the schema's vectors (warm the cache), then exit.
    #[arg(long, hide = HIDE_SEMANTIC)]
    warm: bool,

    /// Print a shell completion script (bash, zsh, fish, ...) to stdout, then exit.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Draft a ready-to-paste example operation for the field the query names
    /// — arguments as variables, one level of leaf fields selected. A query
    /// that only comes close gets the candidate list instead.
    #[arg(short = 'e', long, conflicts_with = "resolve")]
    example: bool,

    /// How many levels of fields --example selects (no effect without it).
    /// Deeper levels expand the object-valued fields level 1 leaves as markers.
    #[arg(long, value_name = "N", default_value_t = 1)]
    depth: usize,

    /// Jump to the graphql-ruby resolver/method for the field the query names,
    /// via `rq` (must be installed). A looser query gets the candidate list.
    #[arg(short = 'R', long)]
    resolve: bool,

    /// Directory of the server code for --resolve (defaults to rq's index).
    #[arg(long)]
    code: Option<String>,

    /// Header for URL introspection, `Name: Value` (repeatable) — e.g. an
    /// `Authorization` token for an auth-gated endpoint.
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    header: Vec<String>,

    /// Print a phase-by-phase timing breakdown to stderr — as JSON when
    /// -j/--json or -J is set, so stdout stays exactly the results.
    #[arg(long)]
    profile: bool,

    /// Verbose stderr diagnostics: cache hits, the rq candidates -R tried, and
    /// (on a semantic build) why the embedding model loaded or fell back.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// Suppress status chatter on stderr (results and hard errors still print).
    #[arg(short, long)]
    quiet: bool,
}

/// The chosen output format — computed once, honored by every mode. Text
/// carries whether to print descriptions; the JSON forms always include them.
#[derive(Clone, Copy)]
enum Output {
    Text { descriptions: bool },
    Json,
    Ndjson,
}

/// All fuzzy hits above the quality cutoff, best first — the caller truncates
/// to the display limit, so the length is the true match count. The `bool` is
/// [`search::named_hit`]: whether some hit names the query's leaf.
fn fuzzy_matches<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    filters: search::Filters<'_>,
) -> (Vec<Match<'a>>, bool) {
    let mut span = crate::profile::span("fuzzy scan");
    let hits = search::search(query, records, filters);
    span.note(|| format!("{} of {} records matched", hits.len(), records.len()));
    let named = search::named_hit(query, &hits);
    let matches = hits
        .into_iter()
        .map(|h| Match {
            record: h.record,
            score: h.score as f64,
        })
        .collect();
    (matches, named)
}

/// Rank by meaning, building the session on first use and holding it in
/// `session` for the queries that follow. One query pays the model load either
/// way; a batch pays it once instead of once per line.
#[cfg(feature = "_semantic")]
fn semantic_matches<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    filters: search::Filters<'_>,
    cli: &Cli,
    session: &mut Option<crate::semantic::Session>,
    schema_key: u64,
    workload: crate::semantic::Workload,
) -> Vec<Match<'a>> {
    let session = session.get_or_insert_with(|| {
        crate::semantic::Session::new(
            records,
            cli.model.as_deref(),
            cli.refresh,
            schema_key,
            workload,
        )
    });
    session
        .rank(query, records, filters, cli.limit)
        .into_iter()
        .map(|(score, record)| Match { record, score })
        .collect()
}

/// Merge the fuzzy and semantic rankings via Reciprocal Rank Fusion — precise
/// name matches and meaning matches both surface, and a record strong in both
/// rises to the top. Fuzzy is weighted a touch higher so an exact-name hit
/// keeps the lead; scale-free, so the two score systems needn't be normalized.
#[cfg(feature = "_semantic")]
fn combine<'a>(fuzzy: Vec<Match<'a>>, semantic: Vec<Match<'a>>, limit: usize) -> Vec<Match<'a>> {
    use std::collections::HashMap;
    const K: f64 = 60.0;
    // Key on the record's stable qualified path (unique per entity) rather than
    // pointer identity, so fusion stays correct even if a ranker ever returned
    // records not borrowed from the same slice.
    let mut scored: HashMap<&str, (f64, &SchemaRecord)> = HashMap::new();
    for (rank, m) in fuzzy.iter().enumerate() {
        scored
            .entry(m.record.path.as_str())
            .or_insert((0.0, m.record))
            .0 += 1.0 / (K + rank as f64 + 1.0);
    }
    for (rank, m) in semantic.iter().enumerate() {
        scored
            .entry(m.record.path.as_str())
            .or_insert((0.0, m.record))
            .0 += 0.7 / (K + rank as f64 + 1.0);
    }
    let mut merged: Vec<Match> = scored
        .into_values()
        .map(|(score, record)| Match { record, score })
        .collect();
    merged.sort_by(|a, b| b.score.total_cmp(&a.score));
    merged.truncate(limit);
    merged
}

/// Spawn a detached `gqls --warm <source>` so the schema's vectors embed in the
/// background — the next run gets combined fuzzy+semantic results with no wait.
/// Opt out with `GQLS_NO_AUTOWARM`. Best-effort; failures are ignored.
#[cfg(feature = "_semantic")]
fn spawn_background_warm(source: &str, headers: &[String]) {
    if std::env::var_os("GQLS_NO_AUTOWARM").is_some() {
        return;
    }
    // Single-flight: a detached warm for this source may already be running.
    // A short-lived lockfile keeps a burst of cold queries from spawning a herd
    // that all embed the same schema and race the cache.
    if !claim_warm_lock(source) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--warm").arg(source);
        for h in headers {
            cmd.arg("--header").arg(h);
        }
        let _ = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Best-effort single-flight guard for background warming: returns true (and
/// stakes a claim) when no recent warm for `source` is in flight, false when one
/// likely is. The lockfile self-expires by mtime, so a crashed warm can't wedge
/// warming forever, and a failed warm won't be retried in a tight loop.
#[cfg(feature = "_semantic")]
fn claim_warm_lock(source: &str) -> bool {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::Duration;

    const LOCK_TTL: Duration = Duration::from_secs(10 * 60);
    // Lockfiles live in the system temp dir so the OS auto-reaps them.
    let dir = crate::paths::temp_dir();
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    let lock = dir.join(format!("warming-{:016x}.lock", h.finish()));
    if let Ok(meta) = std::fs::metadata(&lock) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().is_ok_and(|age| age < LOCK_TTL) {
                return false; // a recent warm is presumably still running
            }
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(&lock, []).is_ok()
}

/// Parse `-H "Name: Value"` strings into `(name, value)` pairs.
fn parse_headers(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|h| {
            let (name, value) = h
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("--header {h:?} must be `Name: Value`"))?;
            Ok((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

pub fn run() -> Result<()> {
    let started = std::time::Instant::now();
    let cli = Cli::parse();
    crate::logging::init(cli.verbose, cli.quiet);
    if cli.profile {
        crate::profile::enable();
    }

    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    if cli.clear_cache {
        let introspect = crate::load::introspect::clear_cache();
        let records = crate::load::record_cache::clear();
        let discoveries = crate::load::discover_cache::clear();
        #[cfg(feature = "_semantic")]
        let vectors = crate::semantic::clear_cache();
        #[cfg(not(feature = "_semantic"))]
        let vectors = 0;
        crate::status!(
            "cleared {} cached file(s)",
            introspect + records + discoveries + vectors
        );
        return Ok(());
    }

    let output = if cli.json {
        Output::Json
    } else if cli.ndjson {
        Output::Ndjson
    } else {
        Output::Text {
            descriptions: !cli.no_description,
        }
    };

    let explicit_kind: Option<Kind> = match &cli.kind {
        Some(s) => Some(s.parse()?),
        None => None,
    };

    // The schema source. With `--warm` and no explicit source, the sole
    // positional is the schema (there's no query to warm), so `gqls --warm
    // schema.graphql` — and the background spawn — target the right file.
    // `--returns` needs no QUERY of its own, so a lone positional beside it is
    // the schema rather than a query — `gqls --returns Company schema.graphql`
    // reads the way it looks. Same shape as the `--warm` rule.
    // Queries arriving on stdin leave the sole positional nothing to be but the
    // schema — the same shape as the `--warm` and `--returns` rules above. A
    // positional that doesn't look like a source stays a query, so an explicit
    // query still beats a pipe rather than being silently ignored.
    let piped = {
        use std::io::IsTerminal;
        !std::io::stdin().is_terminal()
    };
    let (positional_query, positional_source) =
        split_positionals(&cli.args, cli.warm || cli.returns.is_some() || piped);

    let source = match positional_source {
        Some(s) => s,
        None => load::discover(cli.refresh)?,
    };
    let load_opts = load::LoadOptions {
        headers: parse_headers(&cli.header)?,
        refresh: cli.refresh,
    };
    let t_load = std::time::Instant::now();
    let records = {
        let mut span = crate::profile::span("load");
        let records = load::load(&source, &load_opts)?;
        span.note(|| format!("{} records", records.len()));
        records
    };
    crate::detail!(
        "loaded {} records in {:.1?}",
        records.len(),
        t_load.elapsed()
    );

    // --warm: embed + cache the schema's vectors, then exit (no query needed).
    // Also the primitive the background auto-warm spawns.
    if cli.warm {
        #[cfg(feature = "_semantic")]
        {
            let n = crate::semantic::warm(&records, cli.model.as_deref(), cli.refresh);
            crate::status!("cached vectors for {n} record(s)");
            return Ok(());
        }
        #[cfg(not(feature = "_semantic"))]
        {
            let _ = (&cli.model, cli.refresh);
            anyhow::bail!(
                "--warm needs a semantic build — install one with \
                 `cargo install gqls-cli --features semantic` or \
                 `brew install dpep/tools/gqls`"
            );
        }
    }

    // No query and no pipe is an error, bailed on below — the positionals are
    // `num_args = 0..`, so nothing upstream requires one. With a pipe, every
    // line is a query: the schema, the model and the vectors load once and
    // answer all of them, which is the point of the batch form.
    let batch = positional_query.is_none() && cli.returns.is_none() && piped;
    // A kind the query led with, unless `-k` already said one.
    let (kind, positional_query) = match (explicit_kind, positional_query) {
        (None, Some(q)) => match leading_kind(&q) {
            Some((k, rest)) => {
                // Quoting doesn't undo this — a quoted phrase and separate
                // arguments are the same query, deliberately. Setting -k does,
                // because an explicit kind means the word wasn't one.
                crate::status!(
                    "read {:?} as -k {} (set -k to keep it in the query)",
                    q.split_whitespace().next().unwrap_or_default(),
                    k.as_str()
                );
                (Some(k), Some(rest.to_string()))
            }
            None => (None, Some(q)),
        },
        (k, q) => (k, q),
    };
    let queries: Vec<String> = match positional_query {
        Some(q) => vec![q],
        // `--returns Company` on its own lists everything returning Company
        _ if cli.returns.is_some() => vec!["*".into()],
        _ if batch => read_queries()?,
        _ => anyhow::bail!(
            "no query — pass one as an argument (`gqls user schema.graphql`) \
             or pipe one per line (`cat queries.txt | gqls schema.graphql -J`). \
             See --help."
        ),
    };
    if batch && (cli.resolve || cli.example) {
        anyhow::bail!("--resolve and --example take a single query, not piped input");
    }
    crate::detail!(
        "{} quer{}",
        queries.len(),
        if queries.len() == 1 { "y" } else { "ies" }
    );

    // Built on the first query that needs it, then reused: loading the model is
    // the dominant cost of a semantic query, and paying it per line would undo
    // the batch entirely. Same for the schema's cache identity — hashing every
    // record's embedding text is ~10ms on a 10k-record schema, and it's the
    // same answer for every query in the run.
    #[cfg(feature = "_semantic")]
    let mut session: Option<crate::semantic::Session> = None;
    #[cfg(feature = "_semantic")]
    let mut schema_key: Option<u64> = None;

    for query in &queries {
        let query = query.as_str();
        if batch {
            crate::detail!("query {query:?}");
        }

        // A wildcard query (`User.*`) enumerates rather than searches: the pattern
        // does its own scoping and every match is exact, so the qualifier rewrites
        // and the semantic combine below are both bypassed.
        let pattern = search::glob::is_pattern(query);
        if pattern {
            crate::detail!("wildcard query — enumerating matches for {query:?}");
        }

        // `User name` — a two-word query whose first word exactly names a type —
        // is the qualified form typed with a space. Rewrite it, but remember the
        // loose intent: unlike the dot form, an exact hit here keeps the semantic
        // combine on ("around this", not "exactly this").
        let spaced = (!pattern)
            .then(|| search::spaced_qualifier(query, &records))
            .flatten();
        let loose = spaced.is_some();
        let query = spaced.as_deref().unwrap_or(query);
        if loose {
            crate::detail!("two-word query names a type — searching as {query:?}");
        }

        // A `Type.field` query whose qualifier names a schema type — exactly, or
        // as its unique closest misspelling — becomes a hard filter to that type's
        // members, in every search mode. A silent correction would be confusing,
        // so that case is announced at normal verbosity.
        let parent = (!pattern)
            .then(|| search::parent_filter(query, &records))
            .flatten();
        if let Some(p) = parent {
            let (_, qualifier) = search::score::parse_qualified(query);
            if qualifier.is_some_and(|q| q.eq_ignore_ascii_case(p)) {
                crate::detail!("qualifier {p:?} names a type — restricting to its members");
            } else {
                crate::status!(
                    "no type named {:?} — using closest match {p:?}",
                    qualifier.unwrap_or_default()
                );
            }
        }

        let filters = search::Filters {
            kind,
            parent,
            returns: cli.returns.as_deref(),
        };

        if cli.resolve {
            return run_resolve(
                query,
                &source,
                &records,
                filters,
                cli.code.as_deref(),
                cli.limit,
                output,
            );
        }

        if cli.example {
            return run_example(query, &records, filters, cli.depth, cli.limit, output);
        }

        // `total` is the fuzzy match count before the display limit, so the footer
        // can say how much a raised -l would reveal. Semantic-only mode has no
        // meaningful total (cosine ranks every record), so it never shows one.
        let t_rank = std::time::Instant::now();
        let (mut matches, total): (Vec<Match>, usize) = if cli.fuzzy {
            let (mut fuzzy, _) = fuzzy_matches(query, &records, filters);
            let total = fuzzy.len();
            fuzzy.truncate(cli.limit);
            (fuzzy, total)
        } else if cli.semantic {
            #[cfg(feature = "_semantic")]
            {
                if pattern {
                    crate::status!(
                        "--semantic ranks by meaning and ignores wildcards in {query:?}"
                    );
                }
                let key = *schema_key.get_or_insert_with(|| crate::semantic::schema_key(&records));
                // A cold cache means this session embeds the whole schema
                // before it answers anything, and that fill wants the cores
                // for rayon rather than for one model.
                let workload =
                    match !cli.refresh && crate::semantic::is_cached(key, cli.model.as_deref()) {
                        true => crate::semantic::Workload::Query,
                        false => crate::semantic::Workload::Bulk,
                    };
                let matches =
                    semantic_matches(query, &records, filters, &cli, &mut session, key, workload);
                let total = matches.len();
                (matches, total)
            }
            #[cfg(not(feature = "_semantic"))]
            {
                let _ = (&cli.model, cli.refresh);
                anyhow::bail!(
                    "this build has no semantic search — install it with \
                 `cargo install gqls-cli --features semantic` or `brew install dpep/tools/gqls`"
                );
            }
        } else {
            // Default: combine fuzzy + semantic when the cache is warm; when cold,
            // return fuzzy now and warm the vectors in the background for next time.
            // A strong name hit (exact, or the leaf whole at a word boundary —
            // `name` → `lastName`) skips the combine outright: the user typed a
            // word that exists, so semantic ranking would only append lookalike
            // filler below it (and cost the model load).
            let (mut fuzzy, named) = fuzzy_matches(query, &records, filters);
            let total = fuzzy.len();
            fuzzy.truncate(cli.limit);
            #[cfg(feature = "_semantic")]
            {
                // A wildcard enumerates exact matches, and a strong name hit means
                // fuzzy already found the word typed — neither wants meaning-based
                // lookalikes appended (nor the model load they cost).
                let skip = if pattern {
                    Some("wildcard enumeration")
                } else if named && !loose {
                    Some("strong name match")
                } else {
                    None
                };
                if let Some(why) = skip {
                    crate::detail!("{why} — semantic ranking skipped (--semantic to force)");
                    (fuzzy, total)
                } else if crate::semantic::is_cached(
                    *schema_key.get_or_insert_with(|| crate::semantic::schema_key(&records)),
                    cli.model.as_deref(),
                ) {
                    let key = schema_key.expect("computed by the check above");
                    // Warm by the check above, so this session only answers
                    // queries — it can have the cores.
                    let semantic = semantic_matches(
                        query,
                        &records,
                        filters,
                        &cli,
                        &mut session,
                        key,
                        crate::semantic::Workload::Query,
                    );
                    (combine(fuzzy, semantic, cli.limit), total)
                } else {
                    spawn_background_warm(&source, &cli.header);
                    crate::status!(
                        "building the semantic index in the background; next run ranks by \
                     meaning (--semantic to wait, --fuzzy to skip)"
                    );
                    (fuzzy, total)
                }
            }
            #[cfg(not(feature = "_semantic"))]
            {
                let _ = (&cli.model, cli.refresh, named, loose);
                (fuzzy, total)
            }
        };

        crate::detail!("ranked in {:.1?}", t_rank.elapsed());
        let out_span = crate::profile::span("output");

        if matches.is_empty() {
            crate::status!("no matches for {query:?}");
        }
        // Explain mode: the query named exactly one of the records it matched,
        // so the user has found the thing rather than narrowed toward it.
        //
        // Uniqueness among the *named* records, not among all of them. `Role`
        // matches the enum, `User.role` and `CreateUserInput.role`, but names
        // only the enum — casing is what separates them, and GraphQL convention
        // makes that reliable. `role` names all three and stays a list, which is
        // what an ambiguous query should get.
        let explained = (!batch && !cli.no_explain)
            .then(|| explained_match(query, &matches))
            .flatten();
        if let Some((i, _)) = explained {
            // Everything else matched the letters without being what was asked
            // for. Say how many rather than dropping them silently.
            if matches.len() > 1 {
                let others = matches.len() - 1;
                crate::status!(
                    "{others} other match{} for {query:?} (--no-explain to list them)",
                    if others == 1 { "" } else { "es" }
                );
            }
            matches = vec![matches[i]];
        }
        let explained = explained.map(|(_, m)| m);
        output.write_matches(&matches, batch.then_some(query), explained, &records)?;
        drop(out_span);
        if total > matches.len() {
            crate::detail!(
                "{total} matches; showing top {} (-l to adjust)",
                matches.len()
            );
        }
    }

    if crate::profile::enabled() {
        // Always to stderr, so stdout stays exactly the results — and as JSON
        // when the caller asked for JSON, so a baseline can be stored and
        // diffed rather than eyeballed.
        match output {
            Output::Json | Output::Ndjson => {
                eprintln!("{}", crate::profile::json(started.elapsed()));
            }
            Output::Text { .. } => {
                for line in crate::profile::report(started.elapsed()) {
                    eprintln!("{line}");
                }
            }
        }
    }
    Ok(())
}

/// Queries piped on stdin, one per line. Blank lines are skipped so a trailing
/// newline or a padded list doesn't produce an empty search.
fn read_queries() -> Result<Vec<String>> {
    use std::io::BufRead;
    let mut out = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let q = line.trim();
        if !q.is_empty() {
            out.push(q.to_string());
        }
    }
    if out.is_empty() {
        // Reached whenever stdin isn't a terminal and nothing came down it —
        // CI, a script, an agent session — which is exactly where "pipe one per
        // line" is unhelpful on its own, because the reader may simply not have
        // known a positional query was an option. Say both ways.
        anyhow::bail!(
            "no query — pass one as an argument (`gqls user schema.graphql`) \
             or pipe one per line (`cat queries.txt | gqls schema.graphql -J`). \
             See --help."
        );
    }
    Ok(out)
}

impl Output {
    /// `label` is the originating query, set only in batch mode: with many
    /// queries answered on one stream a consumer can't otherwise tell whose
    /// rows are whose. Absent for a single query, so the shape a lone search
    /// emits is exactly what it always was.
    fn write_matches(
        self,
        matches: &[Match],
        label: Option<&str>,
        explained: Option<search::NameMatch>,
        records: &[SchemaRecord],
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Row<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            query: Option<&'a str>,
            #[serde(flatten)]
            record: &'a SchemaRecord,
            score: f64,
            /// `"exact"` or `"corrected"` on the one record a query named, and
            /// absent otherwise — the discriminator for "this response is an
            /// explanation, not a list". Additive, so the array shape and every
            /// existing field stay exactly as they were.
            #[serde(skip_serializing_if = "Option::is_none")]
            r#match: Option<&'static str>,
            /// The same facts the text explanation shows — an enum's values,
            /// what references a type. Flattened in beside the record's own
            /// fields, since to a consumer they're all just what gqls knows.
            #[serde(flatten)]
            extras: Extras<'a>,
        }
        let rows = || {
            matches.iter().map(|m| Row {
                query: label,
                record: m.record,
                score: m.score,
                r#match: explained.map(|m| match m {
                    search::NameMatch::Exact => "exact",
                    search::NameMatch::Corrected => "corrected",
                }),
                extras: match explained {
                    Some(_) => render::extras(m.record, records),
                    None => Extras::default(),
                },
            })
        };
        // A query that matched nothing would otherwise vanish from the stream,
        // leaving the consumer unable to tell it was even asked. Structured
        // output says so explicitly; text mode already says it on stderr.
        if matches.is_empty() {
            if let Some(q) = label {
                let miss = serde_json::json!({ "query": q, "status": "no_matches" });
                match self {
                    Output::Json => println!("{}", serde_json::to_string_pretty(&miss)?),
                    Output::Ndjson => println!("{}", serde_json::to_string(&miss)?),
                    Output::Text { .. } => {}
                }
                return Ok(());
            }
        }
        match self {
            Output::Json => println!(
                "{}",
                serde_json::to_string_pretty(&rows().collect::<Vec<_>>())?
            ),
            Output::Ndjson => {
                for row in rows() {
                    println!("{}", serde_json::to_string(&row)?);
                }
            }
            Output::Text { descriptions } => {
                render::print_text(matches, descriptions, explained.map(|_| records))
            }
        }
        Ok(())
    }
}

/// The one match a query named, if exactly one qualifies — its index and how
/// exactly it was named.
///
/// Case-sensitive names win outright when there are any: `Role` naming the enum
/// exactly takes precedence over the case-insensitive way it also names
/// `User.role`. With no exact-cased name, every named record counts, so a query
/// that names several stays a search.
fn explained_match(query: &str, matches: &[Match]) -> Option<(usize, search::NameMatch)> {
    let named: Vec<usize> = (0..matches.len())
        .filter(|&i| search::names_the_record(query, matches[i].record).is_some())
        .collect();
    let cased: Vec<usize> = named
        .iter()
        .copied()
        .filter(|&i| search::names_the_record_exactly(query, matches[i].record))
        .collect();
    let candidates = if cased.is_empty() { named } else { cased };
    match candidates.as_slice() {
        [only] => search::names_the_record(query, matches[*only].record).map(|m| (*only, m)),
        _ => None,
    }
}

/// The one record a `-e`/`-R` run acts on: the top hit, but only when the query
/// named it. Ranking always has a favourite, and both commands turn that
/// favourite into something that reads as authoritative — an operation to paste,
/// a file and line to open. Where the query was merely *closest* to a field, the
/// candidates are printed instead and the pick handed back to the user.
fn one_named_record<'a>(
    query: &str,
    hits: &[search::Hit<'a>],
    action: &str,
    limit: usize,
    output: Output,
) -> Result<&'a SchemaRecord> {
    let Some(top) = hits.first() else {
        anyhow::bail!("no schema entity matches {query:?} to {action}");
    };
    // Both messages are part of the answer rather than commentary on it, so
    // they print unprefixed, above what they introduce.
    match search::names_the_record(query, top.record) {
        Some(search::NameMatch::Exact) => {}
        // The user typed something else; say which field this is before
        // answering as if they'd asked for it.
        Some(search::NameMatch::Corrected) => {
            ask(format!("Did you mean {}?", top.record.path));
        }
        // Nothing was named: the candidates are the answer, and picking one is
        // the user's call.
        None => {
            ask("Did you mean:");
            let matches: Vec<Match> = hits
                .iter()
                .take(limit)
                .map(|h| Match {
                    record: h.record,
                    score: h.score as f64,
                })
                .collect();
            // A candidate list by construction: this path exists because the
            // query did *not* name a record, so there's nothing to explain.
            output.write_matches(&matches, None, None, &[])?;
            return Err(Handled.into());
        }
    }
    Ok(top.record)
}

/// Put a question to the reader, above the output that answers it. Unprefixed,
/// because it's addressed to them rather than logged at them — but on stderr
/// with the rest of what gqls *says*, so that stdout stays exactly what gqls
/// *produced*: a draft that survives `> op.graphql`, JSON that survives `| jq`.
/// A terminal shows both anyway, which is where these are read. `-q` speaks for
/// a caller that wants results and nothing else.
fn ask(question: impl AsRef<str>) {
    if !crate::logging::is_quiet() {
        eprintln!("{}\n", question.as_ref());
    }
}

/// The run is over and the user has already been told everything they need —
/// only the exit status is left to set. Carrying it as an error keeps that
/// status honest (nothing was drafted or resolved) without printing a second,
/// redundant line under the answer.
#[derive(Debug)]
pub struct Handled;

impl std::fmt::Display for Handled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("handled")
    }
}

impl std::error::Error for Handled {}

/// Find the field, then draft an operation that calls it.
fn run_example(
    query: &str,
    records: &[SchemaRecord],
    filters: search::Filters<'_>,
    depth: usize,
    limit: usize,
    output: Output,
) -> Result<()> {
    let hits = search::search(query, records, filters);
    let target = one_named_record(query, &hits, "draft", limit, output)?;
    crate::detail!("drafting an operation for {}", target.path);
    let example = crate::example::build(target, records, depth)?;
    if !example.deprecated.is_empty() {
        // Selected anyway and marked inline, but worth saying out loud —
        // pasting a deprecated field is the kind of thing you want to know now.
        crate::status!(
            "deprecated: {} (flagged inline)",
            example.deprecated.join(", ")
        );
    }
    if let Some(via) = &example.via {
        // The operation itself shows which root it nests through, so this is a
        // diagnostic; the runners-up, which the draft can't show, go underneath
        // it in the output instead.
        crate::detail!("reached through {via}");
    }

    let payload = serde_json::json!({
        "path": target.path,
        "operation": example.operation,
        "variables": example.variables,
        "optional_args": example.optional,
        "input_types": example.input_types,
        "deprecated": example.deprecated,
        "paths": example.paths(),
    });
    match output {
        Output::Json => println!("{}", serde_json::to_string_pretty(&payload)?),
        Output::Ndjson => println!("{}", serde_json::to_string(&payload)?),
        Output::Text { .. } => print!("{}", render::render_example(&example)?),
    }
    Ok(())
}

/// The text form: the operation, then what didn't fit inside it. Every section
/// is omitted when it has nothing to say — an empty heading is noise in
/// something meant to be read and pasted.
/// Fuzzy-find the field, then hand it to rq to locate its resolver in code.
fn run_resolve(
    query: &str,
    source: &str,
    records: &[SchemaRecord],
    filters: search::Filters<'_>,
    code: Option<&str>,
    limit: usize,
    output: Output,
) -> Result<()> {
    if code.is_none() {
        crate::status!("searching code in the current directory (--code to search elsewhere)");
    }
    let hits = search::search(query, records, filters);
    let target = one_named_record(query, &hits, "resolve", limit, output)?;
    crate::status!("resolving {} …", target.path);
    // a local file schema (not a URL) enables package-proximity ranking
    let schema_path = (!source.starts_with("http://") && !source.starts_with("https://"))
        .then(|| std::path::Path::new(source))
        .filter(|p| p.exists());
    let hits = crate::resolve::resolve(target, code, schema_path, limit.min(10))?;

    match output {
        Output::Json => println!("{}", serde_json::to_string_pretty(&hits)?),
        Output::Ndjson => {
            for h in &hits {
                println!("{}", serde_json::to_string(h)?);
            }
        }
        Output::Text { .. } => {
            if hits.is_empty() {
                crate::status!(
                    "no code definition found for {} (-v shows what was tried)",
                    target.path
                );
            }
            // Say so when the best we have is a bare name search rather than a
            // graphql-ruby convention: an unlabelled list invites trusting a
            // top-ranked guess, which is worse than offering nothing.
            if hits.first().is_some_and(|h| h.loose) {
                crate::status!(
                    "no graphql-ruby convention matched {} — these are name-similarity \
                     guesses, not resolver lookups",
                    target.path
                );
            }
            for h in &hits {
                let flag = if h.loose { "  (guess)" } else { "" };
                println!("{}:{}  {}  (via {}){flag}", h.file, h.line, h.name, h.via);
            }
        }
    }
    Ok(())
}

/// Split the positionals into a query and a schema source.
///
/// The source is recognised by its shape — an extension or a URL — wherever it
/// sits, so `gqls user schema.graphql` and `gqls schema.graphql user` both
/// read. Everything else joins into one query, which is what lets
/// `gqls cancel a subscription` work without quotes.
///
/// `lone_is_source` decides the one genuinely ambiguous case: a single
/// positional that looks like a schema. It's the source when something else
/// supplies the query (`--warm`, `--returns`, a pipe) and the query otherwise,
/// because `gqls schema.graphql` with nothing else is a search for that text.
fn split_positionals(args: &[String], lone_is_source: bool) -> (Option<String>, Option<String>) {
    let source_at = args.iter().rposition(|a| looks_like_source(a));
    let source_at = match source_at {
        Some(_) if args.len() == 1 && !lone_is_source => None,
        other => other,
    };
    let source = source_at.map(|i| args[i].clone());
    let query: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != source_at)
        .map(|(_, a)| a.as_str())
        .collect();
    match query.is_empty() {
        true => (None, source),
        false => (Some(query.join(" ")), source),
    }
}

/// A kind the query leads with — `query user`, `type User`, `enums` — and the
/// rest of the query after it.
///
/// Typing the kind is how people say it out loud, and it reads the same whether
/// the shell split it into arguments or not. Only ever the *first* word, and
/// only when something follows: `gqls query` stays a search for the word.
///
/// This does collide with prose. `gqls input validation` reads `input` as a
/// kind, which is not what a semantic search of that phrase would mean — so the
/// caller says on stderr that it did, and `-k` set explicitly wins outright.
fn leading_kind(query: &str) -> Option<(Kind, &str)> {
    let (first, rest) = query.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    first.parse::<Kind>().ok().map(|k| (k, rest))
}

/// Whether a positional argument is a schema source rather than a query.
/// Syntactic only (no filesystem check): schema sources are URLs or files with
/// a schema extension, none of which is a legal GraphQL name, so this can't
/// swallow a real query.
fn looks_like_source(arg: &str) -> bool {
    arg.starts_with("http://")
        || arg.starts_with("https://")
        || [".graphql", ".graphqls", ".gql", ".json"]
            .iter()
            .any(|ext| arg.to_ascii_lowercase().ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::{leading_kind, looks_like_source, split_positionals};
    use crate::model::Kind;

    #[test]
    fn recognizes_schema_sources_but_not_queries() {
        assert!(looks_like_source("schema.graphql"));
        assert!(looks_like_source("a/b/Schema.GraphQLS"));
        assert!(looks_like_source("dump.json"));
        assert!(looks_like_source("https://api.example.com/graphql"));
        // legal GraphQL names must stay queries
        assert!(!looks_like_source("User.email"));
        assert!(!looks_like_source("Company"));
        assert!(!looks_like_source("User.*"));
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn several_words_are_one_query() {
        let (q, s) = split_positionals(&args(&["cancel", "a", "subscription"]), false);
        assert_eq!(q.as_deref(), Some("cancel a subscription"));
        assert_eq!(s, None);
    }

    #[test]
    fn the_source_is_found_wherever_it_sits() {
        for order in [
            &["user", "schema.graphql"][..],
            &["schema.graphql", "user"][..],
        ] {
            let (q, s) = split_positionals(&args(order), false);
            assert_eq!(q.as_deref(), Some("user"), "{order:?}");
            assert_eq!(s.as_deref(), Some("schema.graphql"), "{order:?}");
        }
    }

    #[test]
    fn a_lone_schema_shaped_argument_depends_on_who_supplies_the_query() {
        // Nothing else to search with: it's the schema.
        let (q, s) = split_positionals(&args(&["schema.graphql"]), true);
        assert_eq!((q.as_deref(), s.as_deref()), (None, Some("schema.graphql")));
        // Otherwise `gqls schema.graphql` is a search for that text, which is
        // what it looks like when you type it.
        let (q, s) = split_positionals(&args(&["schema.graphql"]), false);
        assert_eq!((q.as_deref(), s.as_deref()), (Some("schema.graphql"), None));
    }

    #[test]
    fn a_leading_kind_word_filters() {
        assert_eq!(leading_kind("query user"), Some((Kind::Query, "user")));
        assert_eq!(leading_kind("type User"), Some((Kind::Object, "User")));
        assert_eq!(leading_kind("enums Role"), Some((Kind::Enum, "Role")));
    }

    #[test]
    fn a_kind_word_alone_is_still_a_search() {
        // `gqls query` means "find things called query", not "list every query"
        // — there'd be no way to ask the first if it meant the second.
        assert_eq!(leading_kind("query"), None);
        assert_eq!(leading_kind("mutation  "), None);
    }

    #[test]
    fn only_the_first_word_is_read_as_a_kind() {
        assert_eq!(leading_kind("user query"), None);
    }
}
