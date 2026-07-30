//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::search;

/// The semantic-only flags (--semantic, --model, --refresh, --clear-cache) are
/// hidden from --help on builds without the feature, where they'd only error.
const HIDE_SEMANTIC: bool = !cfg!(feature = "_semantic");

#[cfg(feature = "_semantic")]
const EXAMPLES: &str = "\
EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls User.email                     qualified Type.field query
  gqls 'User.*'                       wildcard — list a type's fields (quote it)
  gqls 'User.{first,last}Name'        also ? for one char, {a,b} to alternate
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
  gqls 'cancel a subscription'        rank by meaning (fuzzy + semantic, auto)
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)
";

#[cfg(not(feature = "_semantic"))]
const EXAMPLES: &str = "\
EXAMPLES:
  gqls user schema.graphql            fuzzy search an SDL file
  gqls createUser -k mutation         restrict to a kind (schema auto-discovered)
  gqls User.email                     qualified Type.field query
  gqls 'User.*'                       wildcard — list a type's fields (quote it)
  gqls 'User.{first,last}Name'        also ? for one char, {a,b} to alternate
  gqls repo schema.json               search a local introspection dump
  gqls repo https://api/graphql       introspect a live endpoint
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
    /// and `Type.field` queries match against the qualified path. Wildcards
    /// (`*` any run, `?` one char, `{a,b}` alternatives) enumerate instead of
    /// searching — quote them (`'User.*'`) so the shell doesn't expand first.
    #[arg(required_unless_present_any = ["clear_cache", "completions", "warm"])]
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
    kind: Option<Kind>,
    parent: Option<&str>,
) -> (Vec<Match<'a>>, bool) {
    let hits = search::search(query, records, kind, parent);
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

#[cfg(feature = "_semantic")]
fn semantic_matches<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    kind: Option<Kind>,
    parent: Option<&str>,
    cli: &Cli,
) -> Vec<Match<'a>> {
    crate::semantic::search(
        query,
        records,
        kind,
        parent,
        cli.limit,
        cli.model.as_deref(),
        cli.refresh,
    )
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
    let cli = Cli::parse();
    crate::logging::init(cli.verbose, cli.quiet);

    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    if cli.clear_cache {
        let introspect = crate::load::introspect::clear_cache();
        let records = crate::load::record_cache::clear();
        #[cfg(feature = "_semantic")]
        let vectors = crate::semantic::clear_cache();
        #[cfg(not(feature = "_semantic"))]
        let vectors = 0;
        crate::status!("cleared {} cached file(s)", introspect + records + vectors);
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
    let source = if let Some(s) = cli.source.clone() {
        s
    } else if cli.warm {
        match cli.query.clone() {
            Some(s) => s,
            None => load::discover()?,
        }
    } else {
        load::discover()?
    };
    let load_opts = load::LoadOptions {
        headers: parse_headers(&cli.header)?,
        refresh: cli.refresh,
    };
    let t_load = std::time::Instant::now();
    let records = load::load(&source, &load_opts)?;
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

    // clap guarantees a query unless --clear-cache/--completions/--warm.
    let query = cli
        .query
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("a QUERY is required (see --help)"))?;

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

    if cli.resolve {
        return run_resolve(
            query,
            &source,
            &records,
            kind,
            parent,
            cli.code.as_deref(),
            cli.limit,
            output,
        );
    }

    // `total` is the fuzzy match count before the display limit, so the footer
    // can say how much a raised -l would reveal. Semantic-only mode has no
    // meaningful total (cosine ranks every record), so it never shows one.
    let t_rank = std::time::Instant::now();
    let (matches, total): (Vec<Match>, usize) = if cli.fuzzy {
        let (mut fuzzy, _) = fuzzy_matches(query, &records, kind, parent);
        let total = fuzzy.len();
        fuzzy.truncate(cli.limit);
        (fuzzy, total)
    } else if cli.semantic {
        #[cfg(feature = "_semantic")]
        {
            if pattern {
                crate::status!("--semantic ranks by meaning and ignores wildcards in {query:?}");
            }
            let matches = semantic_matches(query, &records, kind, parent, &cli);
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
        let (mut fuzzy, named) = fuzzy_matches(query, &records, kind, parent);
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
            } else if crate::semantic::is_cached(&records, cli.model.as_deref()) {
                let semantic = semantic_matches(query, &records, kind, parent, &cli);
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

    if matches.is_empty() {
        crate::status!("no matches for {query:?}");
    }
    output.write_matches(&matches)?;
    if total > matches.len() {
        crate::detail!(
            "{total} matches; showing top {} (-l to adjust)",
            matches.len()
        );
    }
    Ok(())
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

/// Longest description rendered inline. A schema doc can run to paragraphs;
/// one line per result keeps the output greppable, so the rest is elided
/// (`--json` carries the full text).
const DESCRIPTION_WIDTH: usize = 72;

fn print_text(matches: &[Match], descriptions: bool) {
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
        let desc = if descriptions {
            r.description
                .as_deref()
                .map(summarize)
                .filter(|d| !d.is_empty())
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        println!(
            "{path:<width$}{ret}  [{kind}]{dep}{desc}",
            kind = r.kind.as_str()
        );
    }
}

/// A schema description as one line: whitespace (including the newlines of a
/// block description) collapsed, then elided at [`DESCRIPTION_WIDTH`].
fn summarize(description: &str) -> String {
    let mut out = String::new();
    for (i, word) in description.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > DESCRIPTION_WIDTH {
            let kept: String = out.chars().take(DESCRIPTION_WIDTH).collect();
            return format!("{}…", kept.trim_end());
        }
    }
    out
}

/// Fuzzy-find the field, then hand it to rq to locate its resolver in code.
#[allow(clippy::too_many_arguments)]
fn run_resolve(
    query: &str,
    source: &str,
    records: &[SchemaRecord],
    kind: Option<Kind>,
    parent: Option<&str>,
    code: Option<&str>,
    limit: usize,
    output: Output,
) -> Result<()> {
    if code.is_none() {
        crate::status!("searching code in the current directory (--code to search elsewhere)");
    }
    let Some(top) = search::search(query, records, kind, parent)
        .into_iter()
        .next()
    else {
        anyhow::bail!("no schema entity matches {query:?} to resolve");
    };
    crate::status!("resolving {} …", top.record.path);
    // a local file schema (not a URL) enables package-proximity ranking
    let schema_path = (!source.starts_with("http://") && !source.starts_with("https://"))
        .then(|| std::path::Path::new(source))
        .filter(|p| p.exists());
    let hits = crate::resolve::resolve(top.record, code, schema_path, limit.min(10))?;

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

#[cfg(test)]
mod tests {
    use super::{summarize, DESCRIPTION_WIDTH};

    #[test]
    fn collapses_block_descriptions_to_one_line() {
        assert_eq!(summarize("  Look up\n  a user.\n"), "Look up a user.");
        assert_eq!(summarize("   "), "");
    }

    #[test]
    fn elides_past_the_width() {
        let long = "word ".repeat(60);
        let out = summarize(&long);
        assert!(out.ends_with('…'), "{out}");
        assert!(out.chars().count() <= DESCRIPTION_WIDTH + 1, "{out}");
    }

    #[test]
    fn keeps_a_description_that_fits() {
        let s = "An account.";
        assert_eq!(summarize(s), s);
    }
}
