//! clap CLI, dispatch, and output formatting (text / json / ndjson).

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::load;
use crate::model::{Kind, SchemaRecord};
use crate::render::{self, Extras, Match};
use crate::search;

const ABOUT: &str = "Search a GraphQL schema — fuzzy, or straight to the resolver.";

const LONG_ABOUT: &str =
    "Find the types, fields and directives in a GraphQL schema from the terminal. \
     The source is an SDL file, a local introspection JSON dump, or a live http(s) \
     endpoint; with none given, gqls discovers a schema in the current tree. \
     Name one record and gqls explains it rather than listing: its description in full, \
     deprecation, directives, an abstract type's members, an enum's values, a \
     type's fields, and what references it. --example drafts an operation to paste, \
     --resolve jumps to the graphql-ruby resolver via rq. All modes support -j/--json \
     and -J/--ndjson.";

const EXAMPLES: &str = "EXAMPLES:
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
  gqls cancel a subscription          several words are one query
  gqls Mutation.createUser -e         draft an operation to paste
  gqls CreateUserInput -e             ...or draft through the field taking it
  gqls Post.title -e --via posts      ...or pick from the routes it lists
  gqls Query.user -R --code ./app     jump to the graphql-ruby resolver
  gqls user schema.graphql -j         JSON output (-J for ndjson)
";

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
    /// The query is fuzzy-matched: abbreviations like `usr` match `User`,
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

    /// Bypass every cache: re-walk for the schema and re-fetch a URL — for a
    /// change a cache can't see, like a schema that moved.
    #[arg(long)]
    refresh: bool,

    /// Delete every cached file — introspection responses, parsed records,
    /// discovered schema paths, and anything an older release left — then exit.
    #[arg(long)]
    clear_cache: bool,

    /// Print a shell completion script (bash, zsh, fish, ...) to stdout, then exit.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Draft a ready-to-paste example operation for the field the query names
    /// — arguments as variables, one level of leaf fields selected. Name an
    /// input object instead and it drafts through the field that takes it. A
    /// query that only comes close gets the candidate list instead.
    #[arg(short = 'e', long, conflicts_with = "resolve")]
    example: bool,

    /// How many levels of fields --example selects (no effect without it).
    /// Deeper levels expand the object-valued fields level 1 leaves as markers.
    /// Defaults to one level, or to the barest valid selection when the query
    /// names an input object — that draft is about the argument, not the reply.
    /// Capped: a selection set fans out geometrically, and past the cap a draft
    /// is bigger than anything you could paste.
    #[arg(long, value_name = "N")]
    depth: Option<usize>,

    /// Route --example takes, in the notation `# paths` prints:
    /// `Query.repository`, or a chain `Query.repository > Repository.issues`.
    /// Each segment names a field, case-insensitively. For when the shortest
    /// route isn't the one you want — a schema with a global-ID lookup reaches
    /// half its types in one hop through `node(id:)`, and those are the routes
    /// `# paths` then lists.
    #[arg(long, value_name = "PATH", requires = "example")]
    via: Option<String>,

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

    /// Verbose stderr diagnostics: cache hits and the rq candidates -R tried.
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
/// to the display limit, so the length is the true match count.
fn fuzzy_matches<'a>(
    query: &str,
    records: &'a [SchemaRecord],
    filters: search::Filters<'_>,
) -> Vec<Match<'a>> {
    let mut span = crate::profile::span("fuzzy scan");
    let hits = search::search(query, records, filters);
    span.note(|| format!("{} of {} records matched", hits.len(), records.len()));
    hits.into_iter()
        .map(|h| Match {
            record: h.record,
            score: Some(h.score as f64),
        })
        .collect()
}

/// A flag that went with semantic search, if the arguments carry one. Caught
/// before clap, whose unknown-flag tip suggests `-- --fuzzy` — a search for
/// the flag's own text that exits 0. Not declared as hidden args, since those
/// still show up in shell completions.
fn removed_flag(args: impl Iterator<Item = String>) -> Option<String> {
    const REMOVED: [&str; 4] = ["semantic", "fuzzy", "warm", "model"];
    args.take_while(|a| a != "--").find_map(|a| {
        let name = a.strip_prefix("--")?.split('=').next()?.to_string();
        REMOVED.contains(&name.as_str()).then_some(name)
    })
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
    if let Some(flag) = removed_flag(std::env::args().skip(1)) {
        Cli::command()
            .error(
                clap::error::ErrorKind::UnknownArgument,
                format!(
                    "--{flag} was removed along with semantic search — every query is fuzzy \
                     now; drop the flag"
                ),
            )
            .exit();
    }
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
        crate::status!("cleared {} cached file(s)", crate::paths::clear_cache());
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

    // The schema source. `--returns` needs no QUERY of its own, so a lone
    // positional beside it is the schema rather than a query — `gqls --returns
    // Company schema.graphql` reads the way it looks. Queries arriving on stdin
    // leave the sole positional nothing to be but the schema, the same way. A
    // positional that doesn't look like a source stays a query, so an explicit
    // query still beats a pipe rather than being silently ignored.
    let piped = {
        use std::io::IsTerminal;
        !std::io::stdin().is_terminal()
    };
    let (positional_query, positional_source) =
        split_positionals(&cli.args, cli.returns.is_some() || piped);

    // Which schema answered is only worth saying when nobody chose it — see
    // [`no_matches`]. Named the way `-v` names it: the walk starts at the cwd,
    // so the discovered schema is always under it.
    let discovered = positional_source.is_none();
    let source = match positional_source {
        Some(s) => s,
        None => load::discover(cli.refresh)?,
    };
    let discovered_source = discovered.then(|| match std::env::current_dir() {
        Ok(cwd) => load::rel(&cwd, std::path::Path::new(&source)),
        Err(_) => source.clone(),
    });
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

    // No query and no pipe is an error, bailed on below — the positionals are
    // `num_args = 0..`, so nothing upstream requires one. With a pipe, every
    // line is a query: the schema loads once and answers all of them, which is
    // the point of the batch form.
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
    // Lazy, so a batch answers each line as it arrives rather than waiting for
    // the producer to close stdin. Draining first is invisible in the output —
    // the bytes are identical — but it makes `producer | gqls -J` silent until
    // the producer finishes, which is not what a pipe is for.
    let queries: Box<dyn Iterator<Item = Result<String>>> = match positional_query {
        Some(q) => Box::new(std::iter::once(Ok(q))),
        // `--returns Company` on its own lists everything returning Company
        _ if cli.returns.is_some() => Box::new(std::iter::once(Ok("*".into()))),
        _ if batch => Box::new(read_queries()),
        _ => anyhow::bail!(
            "no query — pass one as an argument (`gqls user schema.graphql`) \
             or pipe one per line (`cat queries.txt | gqls schema.graphql -J`). \
             See --help."
        ),
    };
    if batch && (cli.resolve || cli.example) {
        anyhow::bail!("--resolve and --example take a single query, not piped input");
    }
    // `-j` is one complete document per query, and a batch emits several — no
    // parser accepts the concatenation, and the breakage is invisible until a
    // consumer chokes on it. `-J` is the streaming form and always was.
    if batch && matches!(output, Output::Json) {
        anyhow::bail!(
            "-j emits one JSON document per query, which a batch concatenates into \
             something no parser reads — use -J/--ndjson for piped queries, or pass \
             the query as an argument"
        );
    }

    let mut answered = 0usize;
    for query in queries {
        let query = query?;
        let query = query.as_str();
        answered += 1;
        if batch {
            crate::detail!("query {query:?}");
        }

        // A wildcard query (`User.*`) enumerates rather than searches: the pattern
        // does its own scoping and every match is exact, so the qualifier rewrites
        // below are bypassed.
        let pattern = search::glob::is_pattern(query);
        if pattern {
            crate::detail!("wildcard query — enumerating matches for {query:?}");
        }

        // `User name` — a two-word query whose first word exactly names a type —
        // is the qualified form typed with a space.
        let spaced = (!pattern)
            .then(|| search::spaced_qualifier(query, &records))
            .flatten();
        let query = spaced.as_deref().unwrap_or(query);
        if spaced.is_some() {
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

        // Wrappers come off the flag the way they come off the schema: the
        // type as the schema writes it (`[Card!]!`) is what gets pasted back,
        // and it matched nothing. A wildcard has none to peel.
        let returns = cli.returns.as_deref().map(crate::model::base_of);
        // A `--returns` that nothing satisfies outright is widened to what
        // narrows to the type, rather than dead-ending on a precise "no".
        let widened = returns.and_then(|t| search::widened_returns(t, &records));
        if widened.is_some() {
            crate::status!(
                "nothing returns {} outright — showing fields returning a type it narrows from",
                returns.unwrap_or_default()
            );
        }
        let filters = search::Filters {
            kind,
            parent,
            returns: widened.as_deref().or(returns),
        };

        if cli.resolve {
            let done = run_resolve(
                query,
                &source,
                &records,
                filters,
                cli.code.as_deref(),
                cli.limit,
                output,
            );
            emit_profile(started, output);
            return done;
        }

        if cli.example {
            let via = cli.via.as_deref().unwrap_or_default();
            let done = run_example(query, &records, filters, cli.depth, via, cli.limit, output);
            emit_profile(started, output);
            return done;
        }

        // `total` is the match count before the display limit, so the footer
        // can say how much a raised -l would reveal.
        let t_rank = std::time::Instant::now();
        let mut matches = fuzzy_matches(query, &records, filters);
        let total = matches.len();
        matches.truncate(cli.limit);

        crate::detail!("ranked in {:.1?}", t_rank.elapsed());
        let out_span = crate::profile::span("output");

        // Explain mode: the query named exactly one of the records it matched,
        // so the user has found the thing rather than narrowed toward it.
        //
        // Uniqueness among the *named* records, not among all of them. `Role`
        // matches the enum, `User.role` and `CreateUserInput.role`, but names
        // only the enum — casing is what separates them, and GraphQL convention
        // makes that reliable. `role` names all three and stays a list, which is
        // what an ambiguous query should get.
        // A wildcard enumerates rather than searches, so it never explains:
        // `Query.` asks for the fields, not for the type. Unless it enumerated
        // nothing — `SearchHit.` on a union has no members to list, and the
        // union is the answer that does exist.
        let predicate = filters.compile();
        let explained = (!cli.no_explain && (!pattern || matches.is_empty()))
            .then(|| explained_match(query, records.iter().filter(|r| predicate.accepts(r))))
            .flatten();
        if let Some((record, _)) = explained {
            // Everything else matched the letters without being what was asked
            // for. Say how many rather than dropping them silently — out of
            // everything that matched, not out of the page that was displayed.
            if total > 1 {
                let others = total - 1;
                crate::status!(
                    "{others} other match{} for {query:?} (--no-explain to list them)",
                    if others == 1 { "" } else { "es" }
                );
            }
            // The score is whatever ranking gave it, or nothing when ranking
            // put it past `-l` — naming a record explains it either way. Null
            // rather than 0.0: zero is a legal score, so substituting it made
            // `-l` silently rewrite a number consumers sort and threshold on.
            let score = matches
                .iter()
                .find(|m| std::ptr::eq(m.record, record))
                .and_then(|m| m.score);
            matches = vec![Match { record, score }];
        }
        // A miss is nothing matching, not an empty page of matches: `-l 0`
        // reported "no matches" for a query with five of them. Said after the
        // explain decision too, which can answer a query the search itself
        // matched nothing for. `--returns` with no QUERY searches for `*` —
        // gqls's own wildcard, not anything the caller typed — so that case
        // reports the filter they actually gave.
        if total == 0 && explained.is_none() {
            crate::status!(
                "{}",
                no_matches(
                    query,
                    kind,
                    returns,
                    filters,
                    &records,
                    discovered_source.as_deref(),
                )
            );
        }
        let explained = explained.map(|(_, m)| m);
        output.write_matches(&matches, batch.then_some(query), explained, &records)?;
        drop(out_span);
        // Status, not a -v diagnostic: matches were dropped, and a list that
        // simply stops at -l reads as the whole answer. An explanation isn't a
        // truncated list — it says how many it set aside, in its own terms.
        if explained.is_none() && total > matches.len() {
            crate::status!(
                "{total} matches; showing top {} (-l to adjust)",
                matches.len()
            );
        }
    }

    // Reported after the fact: the query count is not knowable up front once
    // the batch is lazy, and an empty pipe is the one case worth saying aloud.
    crate::detail!("{answered} quer{}", if answered == 1 { "y" } else { "ies" });
    if batch && answered == 0 {
        anyhow::bail!(
            "no query — nothing came down the pipe. Pass one as an argument \
             (`gqls user schema.graphql`) or pipe one per line."
        );
    }

    emit_profile(started, output);
    Ok(())
}

/// The profile report, on every path that ends a run. `-e` and `-R` return
/// early, and a `--profile` that silently covers one of the three modes is
/// worse than one that isn't offered.
///
/// Always to stderr, so stdout stays exactly the results — and as JSON when the
/// caller asked for JSON, so a baseline can be stored and diffed rather than
/// eyeballed.
fn emit_profile(started: std::time::Instant, output: Output) {
    if !crate::profile::enabled() {
        return;
    }
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

/// Queries piped on stdin, one per line, yielded as they arrive. Blank lines
/// are skipped so a trailing newline or a padded list doesn't produce an empty
/// search.
///
/// The empty case is handled by the caller rather than here: a lazy iterator
/// cannot know it yielded nothing until the loop is over.
fn read_queries() -> impl Iterator<Item = Result<String>> {
    use std::io::BufRead;
    std::io::stdin()
        .lock()
        .lines()
        .map(|line| line.map_err(anyhow::Error::from))
        .filter_map(|line| match line {
            Ok(line) => {
                let q = line.trim();
                (!q.is_empty()).then(|| Ok(q.to_string()))
            }
            Err(e) => Some(Err(e)),
        })
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
            /// Null where ranking never scored the record — an explanation
            /// stands on the name that was typed, so it survives a `-l` that
            /// sorted the record off the page. The key is always present.
            score: Option<f64>,
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
        // leaving the consumer unable to tell it was even asked — zero rows on
        // a row-per-line stream is zero bytes. So the miss is a row of its own.
        //
        // Only here. `-j` emits one array, and an empty one is already a
        // complete answer; a batch can't reach it at all, since concatenated
        // documents don't parse and the batch refuses `-j` up front. Text says
        // it on stderr.
        if matches.is_empty() && matches!(self, Output::Ndjson) {
            // `query` only in a batch, matching the rows, which carry it only
            // there — with one query there's nothing to tell apart.
            let miss = match label {
                Some(q) => serde_json::json!({ "query": q, "status": "no_matches" }),
                None => serde_json::json!({ "status": "no_matches" }),
            };
            println!("{}", serde_json::to_string(&miss)?);
            return Ok(());
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
/// Takes every record the filters admit, not the displayed page of them:
/// whether a query names one thing is a fact about the schema, and reading it
/// off the top `-l` rows let the display limit decide whether the answer was a
/// list or an explanation — and which record got explained.
fn explained_match<'a>(
    query: &str,
    records: impl Iterator<Item = &'a SchemaRecord>,
) -> Option<(&'a SchemaRecord, search::NameMatch)> {
    let named: Vec<&SchemaRecord> = records
        .filter(|r| search::names_the_record(query, r).is_some())
        .collect();
    let cased: Vec<&SchemaRecord> = named
        .iter()
        .copied()
        .filter(|r| search::names_the_record_exactly(query, r))
        .collect();
    let candidates = if cased.is_empty() { named } else { cased };
    match candidates.as_slice() {
        [only] => search::names_the_record(query, only).map(|m| (*only, m)),
        _ => None,
    }
}

/// Why an empty answer was empty.
///
/// A miss is about the filters as much as the query. "nothing returns Issue"
/// reads as a fact about the schema, and it was false — 43 fields return one,
/// and `-k query` was what emptied the set; a reader reasonably concluded the
/// type was unreachable. So the sentence names the flags in play and, where
/// dropping them would find something, how much.
///
/// `source` is set only when gqls discovered the schema itself. A discovered
/// schema is an assumption baked into every answer, and a miss is the one place
/// it's worth the line: "not in this schema" and "wrong schema" look identical
/// otherwise. Said here rather than on every run, where it would be a line
/// nobody reads on the common path.
fn no_matches(
    query: &str,
    kind: Option<Kind>,
    returns: Option<&str>,
    filters: search::Filters<'_>,
    records: &[SchemaRecord],
    source: Option<&str>,
) -> String {
    // `--returns` with no QUERY searches gqls's own `*`, so the sentence is
    // about the filter rather than about anything the caller typed.
    let about_returns = returns.filter(|_| query == "*");
    let mut subject = match about_returns {
        Some(ty) => format!("nothing returns {ty}"),
        None => format!("no matches for {query:?}"),
    };
    if let Some(s) = source {
        subject += &format!(" in {s}");
    }

    // The flags the sentence doesn't already name, and the search without them.
    // A `parent` stays: it comes from the query's own `Type.` qualifier, so
    // it's part of what was asked rather than something laid over it.
    let mut relaxed = search::Filters {
        parent: filters.parent,
        ..Default::default()
    };
    let mut unnamed = Vec::new();
    match about_returns {
        Some(_) => relaxed.returns = filters.returns,
        None => {
            if let Some(ty) = returns {
                unnamed.push(format!("--returns {ty}"));
            }
        }
    }
    if let Some(k) = kind {
        unnamed.push(format!("-k {}", k.as_str()));
    }
    if unnamed.is_empty() {
        return subject;
    }
    let listed = unnamed.join(" and ");
    // Only reached on a miss, so the second pass costs a run that found nothing.
    match search::search(query, records, relaxed).len() {
        0 => format!("{subject} with {listed}"),
        n => format!(
            "{subject} with {listed} — {n} match{} without {}",
            if n == 1 { "es" } else { "" },
            if unnamed.len() == 1 { "it" } else { "them" },
        ),
    }
}

/// The one record a `-e`/`-R` run acts on: the top hit, but only when the query
/// named it. Ranking always has a favourite, and both commands turn that
/// favourite into something that reads as authoritative — an operation to paste,
/// a file and line to open. Where the query was merely *closest* to a field, the
/// candidates are printed instead and the pick handed back to the user.
///
/// A hit the query spells exactly, casing included, outranks the one that
/// merely scored highest — the same rule [`explained_match`] applies, since
/// ranking itself is case-blind and GraphQL capitalises types and not fields.
/// Without it `gqls Card` explained the type while `gqls Card -e` silently
/// drafted against the field `Mutation.card`.
///
/// Returns the pick and the records that named the query just as exactly.
/// Ranking breaks that tie and always did, but its pick there is arbitrary —
/// `id` names `Character.id`, `Location.id` and `Episode.id` alike — so the
/// runners-up come back to be disclosed rather than dropped. Plain search
/// answers the same ambiguity by listing; erroring out instead would cost a
/// query that works today, and the draft is still the one the user asked for.
fn one_named_record<'a>(
    query: &str,
    hits: &[search::Hit<'a>],
    action: &str,
    limit: usize,
    output: Output,
) -> Result<(&'a SchemaRecord, Vec<&'a str>)> {
    // Best-first, so `find` takes the strongest exact-cased hit where several
    // records share a name (`User.id`, `Post.id`, …) — ranking still breaks
    // those ties, as it always did.
    let top = hits
        .iter()
        .find(|h| search::names_the_record_exactly(query, h.record))
        .or_else(|| hits.first());
    let Some(top) = top else {
        anyhow::bail!("no schema entity matches {query:?} to {action}");
    };
    let mut also = Vec::new();
    // Both messages are part of the answer rather than commentary on it, so
    // they print unprefixed, above what they introduce.
    match search::names_the_record(query, top.record) {
        Some(search::NameMatch::Exact) => {
            also = named_peers(query, hits, top.record);
            // A caveat on an answer still being given, so it goes where the
            // deprecation warning goes rather than joining the two questions
            // above. The names themselves are a list, and plain search already
            // renders one better than a status line can.
            if !also.is_empty() {
                crate::status!(
                    "{query} names {} records; using {}",
                    also.len() + 1,
                    top.record.path
                );
            }
        }
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
                    score: Some(h.score as f64),
                })
                .collect();
            // A candidate list by construction: this path exists because the
            // query did *not* name a record, so there's nothing to explain.
            output.write_matches(&matches, None, None, &[])?;
            return Err(Handled.into());
        }
    }
    Ok((top.record, also))
}

/// The records that name `query` exactly as `pick` does — the tie ranking broke
/// to arrive at `pick`.
///
/// Case is part of the spelling wherever it tells two records apart, so an
/// exact-cased pick ties only with other exact-cased names: `id` ties with
/// `Character.id` and `Location.id`, and not with the `ID` scalar.
fn named_peers<'a>(query: &str, hits: &[search::Hit<'a>], pick: &SchemaRecord) -> Vec<&'a str> {
    let cased = search::names_the_record_exactly(query, pick);
    hits.iter()
        .map(|h| h.record)
        .filter(|r| r.path != pick.path)
        .filter(|r| match cased {
            true => search::names_the_record_exactly(query, r),
            false => matches!(
                search::names_the_record(query, r),
                Some(search::NameMatch::Exact)
            ),
        })
        .map(|r| r.path.as_str())
        .collect()
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
/// A drafted operation past this many bytes is one nobody will paste. Measured
/// across four schemas: the largest draft anyone would use was 8.5KB
/// (`countries` at `--depth 6`), the smallest unusable one 88KB (chime's `User`
/// at `--depth 4`). Anywhere in that decade works; this sits clear of both ends.
const HUGE_DRAFT: usize = 32 * 1024;

fn run_example(
    query: &str,
    records: &[SchemaRecord],
    filters: search::Filters<'_>,
    depth: Option<usize>,
    via: &str,
    limit: usize,
    output: Output,
) -> Result<()> {
    let hits = search::search(query, records, filters);
    let (target, also_named) = one_named_record(query, &hits, "draft", limit, output)?;
    crate::detail!("drafting an operation for {}", target.path);
    // Said out loud rather than clamped quietly: the draft that comes back is
    // not the one that was asked for, and a silent cap reads as a bug in the
    // flag.
    if depth.is_some_and(|d| d > crate::example::MAX_DEPTH) {
        crate::status!(
            "--depth capped at {} (deeper drafts run to hundreds of megabytes)",
            crate::example::MAX_DEPTH
        );
    }
    let example = crate::example::build_via(target, records, depth, via)?;
    if !example.deprecated.is_empty() {
        // Drafted anyway, but worth saying out loud — pasting a deprecated
        // field is the kind of thing you want to know now.
        //
        // Not "flagged inline": the drafted field itself is the one thing here
        // that never carries a note, because the note goes on a selection and
        // the target is the operation. Saying where to look was a promise the
        // output couldn't keep; saying it wasn't dropped is one it can.
        crate::status!(
            "deprecated: {} (drafted anyway)",
            example.deprecated.join(", ")
        );
    }
    // A draft nobody can paste is a draft that didn't answer the question, and
    // the size is invisible until it has already scrolled past. Measured rather
    // than guessed: across four schemas every draft anyone would use came in
    // under 9KB, while the smallest unusable one — chime's `User` at depth 4 —
    // was 88KB. The threshold sits in the order of magnitude between them.
    //
    // Not a cap. A big draft is still the honest answer to what was asked, and a
    // large schema can want one; this only makes the size visible. `--depth` is
    // named only when it was raised, since it's the lever that got you here.
    if example.operation.len() > HUGE_DRAFT {
        let lever = match depth {
            Some(d) if d > 1 => " — a smaller --depth narrows it",
            _ => "",
        };
        crate::status!("this draft is {}KB{lever}", example.operation.len() / 1024);
    }
    if example.no_leaves {
        // The markers name the holes, but nothing says the flag that fills
        // them — and a wrapper type is where most people meet `-e` first.
        crate::status!("every field here returns an object (--depth selects inside them)");
    }
    if let Some(through) = &example.through {
        // The draft passes something larger than what was asked about, and the
        // signature names only that larger thing — so the connection between
        // the two is only in the variables block unless it's said out loud.
        crate::status!("{} is passed inside {through}", target.path);
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
        "passed_inside": example.through,
        // `[[name, what it's for], …]` — ordered like the operation's own
        // arguments, and only the ones the schema documents.
        "arguments": example.arguments,
        "enums": example.enums,
        "variable_types": example.variable_types,
        "deprecated": example.deprecated,
        "paths": example.paths(),
        // The records the query named just as exactly, which ranking chose
        // `path` out of. Empty when it named only one — but always present,
        // because a consumer reading a draft as settled is the whole reason
        // this is here, and an absent key says nothing.
        "also_named": also_named,
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
    // `-R`'s JSON is the resolver hits themselves, with no envelope to carry
    // the runners-up — the status line above is the whole disclosure here.
    let (target, _) = one_named_record(query, &hits, "resolve", limit, output)?;
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
/// supplies the query (`--returns`, a pipe) and the query otherwise,
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
/// kind, which is not what the phrase means — so the caller says on stderr
/// that it did, and `-k` set explicitly wins outright.
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
