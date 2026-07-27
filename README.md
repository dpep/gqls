# gqls

**Fuzzy and semantic search over a GraphQL schema — from the terminal, for very large graphs.**

[![crates.io](https://img.shields.io/crates/v/gqls.svg)](https://crates.io/crates/gqls)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Point `gqls` at a schema — an SDL file, a local introspection dump, or a live
endpoint — and find the type, field, argument, or directive you're after by
approximate name, by meaning, or jump straight to its resolver in code. Built
for schemas too big to scroll (GitHub's ~68k-line API answers in ~0.15s).

```sh
gqls user schema.graphql              # fuzzy: usr, usre, User.email all work
gqls repository https://api/graphql   # introspect a live endpoint
gqls 'cancel a subscription' -s       # semantic: rank by meaning
gqls Query.user -R --code ./app       # jump to the graphql-ruby resolver
```

## Why

Existing tools don't combine fuzzy/semantic search with big-schema speed from a
CLI: schema viewers list and filter but don't fuzzy-match, hosted explorers are
GUIs, and the semantic-search tools are built for agents, not developers. `gqls`
fills that gap and stays Unix-composable (`-j`/`-J` for JSON/NDJSON everywhere).

## Install

```sh
# Homebrew (fuzzy + introspection + resolver jump)
brew install dpep/tools/gqls

# Cargo (crate is `gqls-cli`; it installs the `gqls` binary)
cargo install gqls-cli

# Cargo, with semantic search (downloads ONNX Runtime at build time)
cargo install gqls-cli --features semantic
```

The resolver jump (`-R`) shells out to [`rq`](https://github.com/dpep/rq); install it too if you want that.

## Usage

### Input sources
- **SDL file** — `gqls user schema.graphql`
- **Introspection JSON dump** — `gqls user schema.json`
- **Live endpoint** — `gqls user https://api.example.com/graphql` (POSTs the introspection query)
- **Auto-discovery** — omit the source and gqls finds a schema in the current tree (preferring `.graphqls`, then `schema.*`, then an introspection `.json`, then any SDL-looking `.graphql`).

### Fuzzy search (default)
Abbreviations (`usr` → `User`), transpositions/typos (`usre` → `User`), and
qualified `Type.field` queries all work; results rank by match quality with
root `Query`/`Mutation` fields floated up.

```sh
gqls createUser -k mutation      # restrict to a kind (plurals ok: mutations)
gqls User.email                  # qualified — boosts the field on User
```

### Semantic search (`-s`, requires `--features semantic`)
Ranks by meaning using a local `all-MiniLM-L6-v2` model (via ONNX Runtime),
fetched once from the HuggingFace Hub then cached offline.

```sh
gqls 'delete a repository' -s https://api/graphql
```

Per-record vectors are cached (keyed by schema content + model), so the first
run on a large schema embeds everything once (parallelized across cores) and
later runs only embed the query — GitHub's schema: cold ~40s, warm ~0.3s.
Editing the schema re-embeds automatically; `--refresh` forces it and
`--clear-cache` wipes the cache.

### Resolver jump (`-R`, graphql-ruby)
Find a field, then jump to the resolver/method that implements it, via `rq`:

```sh
$ gqls Query.user schema.graphql -R --code ./app
app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

gqls tries graphql-ruby naming conventions (resolver class, type method,
mutation class) and ranks the candidates.

### Output
Every mode supports `-j`/`--json` (pretty array) and `-J`/`--ndjson` (one record
per line); status chatter goes to stderr, so JSON pipes cleanly:

```sh
gqls repository schema.json -J | jq -r '.path'
```

## Using with Claude Code

Drop a small skill into `~/.claude/skills/gqls/` (see `claude/gqls-skill.md` in
this repo) so Claude reaches for `gqls` when navigating a GraphQL schema instead
of grepping SDL by hand.

## How it works

Layered so the core is one idea — flatten every schema entity to a searchable
record, and let search/output touch nothing but records:

```
src/
  model.rs        SchemaRecord + Kind (the only shared vocabulary)
  load/           SDL parse · introspection (URL/JSON) · schema discovery
  search/         the fuzzy scorer (a DP subsequence aligner + typo tier)
  semantic/       embedding search + on-disk vector cache (feature = "semantic")
  resolve.rs      field -> resolver jump (shells out to rq)
  cli.rs          clap + unified text/json/ndjson output
```

Two capabilities are **borrowed** from sibling tools rather than reinvented: the
fuzzy ranking is ported from [`rq`](https://github.com/dpep/rq)'s aligner, and
the local embedding pipeline is copied from [`ae`](https://github.com/dpep/ae).

## License

MIT — see [LICENSE](LICENSE).
