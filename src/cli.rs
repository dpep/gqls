//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::search;
use crate::style;

/// The semantic-only flags (--semantic, --model, --refresh, --clear-cache) are
/// hidden from --help on builds without the feature, where they'd only error.
const HIDE_SEMANTIC: bool = !cfg!(feature = "_semantic");

#[cfg(feature = "_semantic")]
const EXAMPLES: &str = "\
EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls User.email                     qualified Type.field query
  gqls User.                          list a type's fields (or 'User.*')
  gqls 'User.{first,last}Name'        also ? for one char, {a,b} to alternate
  gqls --returns Company -k query     find fields by return type, not name
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
  gqls 'cancel a subscription'        rank by meaning (fuzzy + semantic, auto)
  gqls Mutation.createUser -e         draft an operation to paste
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)
";

#[cfg(not(feature = "_semantic"))]
const EXAMPLES: &str = "\
EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls User.email                     qualified Type.field query
  gqls User.                          list a type's fields (or 'User.*')
  gqls 'User.{first,last}Name'        also ? for one char, {a,b} to alternate
  gqls --returns Company -k query     find fields by return type, not name
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
  gqls Mutation.createUser -e         draft an operation to paste
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)

Semantic search (--semantic, rank by meaning) is not compiled into this build. Enable it:
  cargo install gqls-cli --features semantic
  brew install dpep/tools/gqls
";

#[derive(Parser)]
#[command(
    name = "gqls",
    version,
    about = "Search a GraphQL schema — fuzzy, semantic, or straight to the resolver.",
    long_about = "Find the types, fields, args, and directives in a GraphQL schema from the \
                  terminal. The source is an SDL file, a local introspection JSON dump, or a live \
                  http(s) endpoint; with none given, gqls discovers a schema in the current tree. \
                  Fuzzy and semantic results are ranked together by default (--semantic or \
                  --fuzzy forces one); --resolve jumps to the graphql-ruby resolver via rq. All modes \
                  support -j/--json and -J/--ndjson.",
    after_help = EXAMPLES
)]
struct Cli {
    /// Search query. Fuzzy by default; abbreviations like `usr` match `User`,
    /// and `Type.field` queries match against the qualified path. A trailing
    /// dot lists a type's fields (`User.`); the general wildcards (`*` any
    /// run, `?` one char, `{a,b}` alternatives) enumerate too, but quote them
    /// so the shell doesn't expand them first.
    /// Omitted with a pipe on stdin, where each line is a query instead.
    query: Option<String>,

    /// Schema source: a `.graphql`/`.graphqls` SDL file, a `.json` introspection
    /// dump, or an http(s) URL (introspected live). If omitted, gqls searches
    /// the current directory tree for a schema.
    source: Option<String>,

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

    /// Force semantic-only search. By default fuzzy and semantic results are
    /// combined once the schema's vectors are cached.
    #[arg(long, hide = HIDE_SEMANTIC)]
    semantic: bool,

    /// Force fuzzy-only search — skip the semantic combine.
    #[arg(long, conflicts_with = "semantic")]
    fuzzy: bool,

    /// Embedding model for --semantic: a local dir / `.onnx` path, or a
    /// HuggingFace `org/name` id. Defaults to all-MiniLM-L6-v2.
    #[arg(long, hide = HIDE_SEMANTIC)]
    model: Option<String>,

    /// Force a re-embed for --semantic, overwriting the cache. Schema edits
    /// already re-embed on their own; use this for changes the cache can't see
    /// (e.g. a new model).
    #[arg(long, hide = HIDE_SEMANTIC)]
    refresh: bool,

    /// Delete all cached embedding vector files, then exit.
    #[arg(long, hide = HIDE_SEMANTIC)]
    clear_cache: bool,

    /// Pre-embed the schema's vectors (warm the cache), then exit.
    #[arg(long, hide = HIDE_SEMANTIC)]
    warm: bool,

    /// Print a shell completion script (bash, zsh, fish, ...) to stdout, then exit.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Draft a ready-to-paste example operation for the top match — arguments
    /// as variables, one level of leaf fields selected.
    #[arg(short = 'e', long, conflicts_with = "resolve")]
    example: bool,

    /// How many levels of fields --example selects. Deeper levels expand the
    /// object-valued fields that level 1 leaves as markers.
    #[arg(long, value_name = "N", default_value_t = 1)]
    depth: usize,

    /// Jump the top match to its graphql-ruby resolver/method in code, via
    /// `rq` (must be installed).
    #[arg(short = 'R', long)]
    resolve: bool,

    /// Directory of the server code for --resolve (defaults to rq's index).
    #[arg(long)]
    code: Option<String>,

    /// Header for URL introspection, `Name: Value` (repeatable) — e.g. an
    /// `Authorization` token for an auth-gated endpoint.
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    header: Vec<String>,

    /// Print a phase-by-phase timing breakdown to stderr (or into --json).
    #[arg(long)]
    profile: bool,

    /// Verbose stderr diagnostics: cache hits, rq candidates, and why the
    /// embedding model loaded or fell back.
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

/// A ranked result — from either the fuzzy scorer or the semantic ranker, so
/// both flow through one output path.
struct Match<'a> {
    record: &'a SchemaRecord,
    score: f64,
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

    let kind: Option<Kind> = match &cli.kind {
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
    let positional_is_source = cli.source.is_none()
        && (cli.returns.is_some() || piped)
        && cli.query.as_deref().is_some_and(looks_like_source);

    let source = if let Some(s) = cli.source.clone() {
        s
    } else if cli.warm || positional_is_source {
        match cli.query.clone() {
            Some(s) => s,
            None => load::discover(cli.refresh)?,
        }
    } else {
        load::discover(cli.refresh)?
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
            anyhow::bail!("--warm needs a semantic build");
        }
    }

    // clap guarantees a query unless --clear-cache/--completions/--warm/--returns.
    // With no query and a pipe on stdin, every line is one — the schema, the
    // model and the vectors are loaded once and answer all of them, which is
    // the whole point of the batch form.
    let batch =
        cli.query.as_deref().is_none_or(|_| positional_is_source) && cli.returns.is_none() && piped;
    let queries: Vec<String> = match cli.query.as_deref() {
        // a positional consumed as the schema above isn't the query
        Some(q) if !positional_is_source => vec![q.to_string()],
        // `--returns Company` on its own lists everything returning Company
        _ if cli.returns.is_some() => vec!["*".into()],
        _ if batch => read_queries()?,
        _ => anyhow::bail!(
            "a QUERY is required — or pipe one per line \
             (`cat queries.txt | gqls schema.graphql -J`). See --help."
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
        let (matches, total): (Vec<Match>, usize) = if cli.fuzzy {
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
        output.write_matches(&matches, batch.then_some(query))?;
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
        anyhow::bail!("no queries on stdin (one per line)");
    }
    Ok(out)
}

impl Output {
    /// `label` is the originating query, set only in batch mode: with many
    /// queries answered on one stream a consumer can't otherwise tell whose
    /// rows are whose. Absent for a single query, so the shape a lone search
    /// emits is exactly what it always was.
    fn write_matches(self, matches: &[Match], label: Option<&str>) -> Result<()> {
        #[derive(Serialize)]
        struct Row<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            query: Option<&'a str>,
            #[serde(flatten)]
            record: &'a SchemaRecord,
            score: f64,
        }
        let rows = || {
            matches.iter().map(|m| Row {
                query: label,
                record: m.record,
                score: m.score,
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
            Output::Text { descriptions } => print_text(matches, descriptions),
        }
        Ok(())
    }
}

/// Lines a description may occupy, its first included. A schema doc can run to
/// paragraphs; three lines is enough for a sentence or two and little enough
/// that a documented result still reads as one entry rather than a paragraph
/// with a heading. `--json` carries the full text.
const DESCRIPTION_LINES: usize = 3;

/// Never squeeze a description below this. On a narrow terminal the columns in
/// front of it can eat the whole line, and wrapping every third word is worse
/// than running past the edge — so past this point it overflows and lets the
/// terminal do what it likes.
const MIN_DESCRIPTION_WIDTH: usize = 24;

/// Widest the path column may grow before it stops aligning. One pathological
/// 200-character path shouldn't indent every other row past the fold, so it's
/// allowed to overflow its own line instead.
const PATH_WIDTH: usize = 48;

/// A row's cells, kept as plain text so the column widths can be measured, and
/// styled only on the way out.
struct Row {
    path: String,
    args: String,
    ret: String,
    kind: String,
    deprecated: bool,
    desc: String,
}

impl Row {
    /// Visible width of the path cell — the path and its arguments share one
    /// column. Counted in chars: GraphQL names are ASCII by spec, but the
    /// collapsed argument marker is an ellipsis, three bytes to one column.
    fn path_width(&self) -> usize {
        self.path.chars().count() + self.args.chars().count()
    }
}

fn print_text(matches: &[Match], descriptions: bool) {
    // With one result there is no table: nothing else pays for a long signature,
    // and there's no column to align a description against. Both of those turn
    // into "show the whole thing" below.
    let lone = matches.len() == 1;

    let rows: Vec<Row> = matches
        .iter()
        .map(|m| {
            let r = m.record;
            Row {
                path: r.path.clone(),
                // Collapsed in a list: the longest signature would otherwise
                // set the path column width for every row — 44 columns against
                // 22 on the example schema — and `-e` answers "how do I call
                // this" properly anyway. Alone, nothing else pays for it.
                args: match (r.args.is_empty(), lone) {
                    (true, _) => String::new(),
                    (false, true) => format!("({})", r.args.join(", ")),
                    (false, false) => "(…)".to_string(),
                },
                ret: r
                    .type_ref
                    .as_deref()
                    .map(|t| format!("-> {t}"))
                    .unwrap_or_default(),
                kind: format!("[{}]", r.kind.as_str()),
                deprecated: r.deprecated.is_some(),
                desc: match descriptions {
                    true => r.description.clone().unwrap_or_default(),
                    false => String::new(),
                },
            }
        })
        .collect();

    // Measure every column before printing any of it. A column that's empty
    // across the whole result set is dropped rather than left as a blank gutter
    // — a search returning only types has no return-type column at all.
    let path_w = rows
        .iter()
        .map(Row::path_width)
        .max()
        .unwrap_or(0)
        .min(PATH_WIDTH);
    let ret_w = rows.iter().map(|r| r.ret.len()).max().unwrap_or(0);
    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(0);

    for row in &rows {
        let mut line = style::Line::default();
        line.push(&row.path, style::name);
        line.push(&row.args, style::muted);
        if ret_w > 0 {
            line.pad_to(path_w);
            line.gap();
            line.push(&row.ret, style::muted);
        }
        if kind_w > 0 {
            line.pad_to(if ret_w > 0 { ret_w } else { path_w });
            line.gap();
            line.push(&row.kind, style::muted);
        }
        // Appended rather than given a column of its own: one deprecated row
        // would otherwise widen the kind column by 13 for every row.
        if row.deprecated {
            line.pad_to(kind_w);
            line.gap();
            line.push("(deprecated)", style::warning);
        }

        if row.desc.is_empty() {
            println!("{}", line.finish());
            continue;
        }

        // A lone result gets the whole description, at full width, on its own
        // lines. Not just an uncapped version of the inline form: hanging the
        // full text off a 64-column indent wraps it into a tall ribbon a few
        // words wide, which is worse than the truncation it replaces.
        if lone {
            println!("{}", line.finish());
            for l in wrap(&row.desc, style::width().saturating_sub(2), usize::MAX) {
                println!("  {}", style::muted(&l));
            }
            continue;
        }

        // Otherwise it hangs off the row, its continuations indented to where
        // it starts so a wrapped description reads as one block rather than as
        // a new result at column 0.
        line.pad_to(if kind_w > 0 { kind_w } else { path_w });
        line.gap();
        let indent = line.width();
        let budget = style::width()
            .saturating_sub(indent)
            .max(MIN_DESCRIPTION_WIDTH);
        // "— " belongs to the first line only; continuations align past it, so
        // the prose edges line up rather than the dash.
        let mut lines = wrap(&row.desc, budget.saturating_sub(2), DESCRIPTION_LINES).into_iter();
        if let Some(first) = lines.next() {
            line.push(&format!("— {first}"), style::muted);
        }
        println!("{}", line.finish());
        for cont in lines {
            println!("{}{}", " ".repeat(indent + 2), style::muted(&cont));
        }
    }
}

/// Break `text` into at most `max_lines` lines of `width` columns, on word
/// boundaries, marking the end with `…` when there was more.
///
/// A word longer than the whole width (a URL, a long type name) is left to
/// overflow rather than cut mid-token: breaking it produces two fragments that
/// are each unsearchable, and the thing that overflows is a dim tail.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        if !current.is_empty() && current.chars().count() + extra > width {
            if lines.len() + 1 == max_lines {
                // No room for another line: elide, trimming enough to fit the
                // marker rather than pushing one column past the budget.
                let mut kept = current;
                while kept.chars().count() + 1 > width {
                    kept.pop();
                }
                lines.push(format!("{}…", kept.trim_end()));
                return lines;
            }
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
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
            output.write_matches(&matches, None)?;
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
        Output::Text { .. } => print!("{}", render_example(&example)?),
    }
    Ok(())
}

/// The text form: the operation, then what didn't fit inside it. Every section
/// is omitted when it has nothing to say — an empty heading is noise in
/// something meant to be read and pasted.
fn render_example(example: &crate::example::Example) -> Result<String> {
    let mut out = String::new();

    // What the field does, above the operation that calls it. `-e` has already
    // committed to one field — it refuses to draft unless the query named it —
    // so there's no list to keep short here, and the whole description goes in.
    // A comment, like every other annotation in this output, so the block stays
    // pasteable in one go.
    if let Some(description) = example.description.as_deref() {
        for line in wrap(description, style::width().saturating_sub(2), usize::MAX) {
            out.push_str(&format!("# {line}\n"));
        }
        out.push('\n');
    }
    out.push_str(&example.operation);

    if !example.optional.is_empty() {
        out.push_str("\n# optional arguments:\n");
        for arg in &example.optional {
            out.push_str(&format!("#   {arg}\n"));
        }
    }
    if !example.input_types.is_empty() {
        out.push_str("\n# input types:\n");
        for line in example.input_types.iter().flatten() {
            out.push_str(&format!("#   {line}\n"));
        }
    }
    // An operation with no required arguments takes no variables; printing an
    // empty `{}` under a heading only invites the reader to look for something.
    if example.variables.as_object().is_some_and(|v| !v.is_empty()) {
        out.push_str("\n# variables\n");
        out.push_str(&format!(
            "{}\n",
            serde_json::to_string_pretty(&example.variables)?
        ));
    }
    // Every root that reaches the target, the drafted one first — so the pick
    // reads as a choice among the paths rather than as the only one. A single
    // path is already shown by the operation itself, and one entry under a
    // heading says nothing the draft didn't.
    let paths = example.paths();
    if paths.len() > 1 {
        out.push_str("\n# paths:\n");
        for path in paths {
            out.push_str(&format!("#   {path}\n"));
        }
    }
    Ok(out)
}

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
    use super::{looks_like_source, render_example, wrap};
    use crate::example::Example;

    fn example() -> Example {
        Example {
            operation: "query Users {\n  users {\n    email\n  }\n}\n".to_string(),
            description: None,
            variables: serde_json::json!({}),
            optional: Vec::new(),
            input_types: Vec::new(),
            deprecated: Vec::new(),
            via: None,
            alternatives: Vec::new(),
        }
    }

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

    #[test]
    fn collapses_block_descriptions_to_one_line() {
        assert_eq!(
            wrap("  Look up\n  a user.\n", 40, 3),
            vec!["Look up a user."]
        );
        assert!(wrap("   ", 40, 3).is_empty());
    }

    #[test]
    fn wraps_on_word_boundaries_within_the_budget() {
        let out = wrap("the quick brown fox jumps over the lazy dog", 12, 9);
        assert!(
            out.iter().all(|l| l.chars().count() <= 12),
            "over budget: {out:?}"
        );
        assert_eq!(out.join(" "), "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn elides_once_it_runs_out_of_lines() {
        let long = "word ".repeat(60);
        let out = wrap(&long, 20, 3);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out.last().unwrap().ends_with('…'), "{out:?}");
        // The marker has to fit the budget, not sit one column past it.
        assert!(
            out.iter().all(|l| l.chars().count() <= 20),
            "over budget: {out:?}"
        );
    }

    #[test]
    fn a_description_that_fits_stays_on_one_line() {
        let out = wrap("An account.", 40, 3);
        assert_eq!(out, vec!["An account.".to_string()]);
    }

    #[test]
    fn a_word_longer_than_the_budget_overflows_rather_than_splitting() {
        // Splitting a URL or a long type name yields two unsearchable halves.
        let out = wrap("see https://example.com/a/very/long/path now", 12, 3);
        assert!(
            out.iter().any(|l| l.contains("https://example.com")),
            "{out:?}"
        );
    }

    #[test]
    fn an_operation_with_nothing_to_add_is_printed_alone() {
        let ex = example();
        assert_eq!(render_example(&ex).unwrap(), ex.operation);
    }

    #[test]
    fn a_description_heads_the_draft_as_a_comment() {
        let mut ex = example();
        ex.description = Some("Look up a user by id.".to_string());
        let out = render_example(&ex).unwrap();
        assert!(
            out.starts_with("# Look up a user by id.\n\nquery Users {"),
            "{out}"
        );
        // Commented, so the whole block still pastes as one document.
        graphql_parser::parse_query::<String>(&out).expect("draft should stay valid");
    }

    #[test]
    fn a_long_description_is_wrapped_and_every_line_commented() {
        let mut ex = example();
        ex.description = Some("word ".repeat(60));
        let out = render_example(&ex).unwrap();
        let heading: Vec<&str> = out.lines().take_while(|l| l.starts_with('#')).collect();
        assert!(heading.len() > 3, "expected a wrapped block: {out}");
        // Uncapped — `-e` has already committed to one field, so there's no
        // list for a long description to bury.
        assert!(!out.contains('…'), "should not elide in a draft: {out}");
        graphql_parser::parse_query::<String>(&out).expect("draft should stay valid");
    }

    #[test]
    fn variables_are_printed_only_when_there_are_some() {
        let mut ex = example();
        ex.variables = serde_json::json!({ "id": "<ID!>" });
        let out = render_example(&ex).unwrap();
        assert!(
            out.contains("# variables\n{\n  \"id\": \"<ID!>\"\n}\n"),
            "{out}"
        );
    }

    #[test]
    fn the_roots_that_reach_the_target_are_listed_with_the_drafted_one_first() {
        let mut ex = example();
        ex.via = Some("Query.users".to_string());
        ex.alternatives = vec!["Query.user".to_string()];
        let out = render_example(&ex).unwrap();
        assert!(
            out.ends_with("\n# paths:\n#   Query.users\n#   Query.user\n"),
            "{out}"
        );
    }

    #[test]
    fn a_lone_path_is_left_to_the_operation_to_show() {
        let mut ex = example();
        ex.via = Some("Query.users".to_string());
        let out = render_example(&ex).unwrap();
        assert!(!out.contains("# paths"), "{out}");
    }
}
