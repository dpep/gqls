# gqls

**Fuzzy and semantic search over a GraphQL schema — from the terminal, for very large graphs.**

[![crates.io](https://img.shields.io/crates/v/gqls-cli.svg)](https://crates.io/crates/gqls-cli)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Point `gqls` at a schema and find the type, field, argument, or directive you're after — by approximate name, by meaning, or by jumping straight to its resolver in code. It reads an SDL file, an introspection dump, a live endpoint, or a federated supergraph, so instead of grepping SDL and guessing the exact spelling you get ranked matches — even on schemas too big to scroll, where GitHub's ~68k-line API answers in ~0.15s.

```sh
gqls user schema.graphql              # fuzzy: usr, usre, User.email all match
gqls repository https://api/graphql   # introspect a live endpoint
gqls 'cancel a subscription'          # ranks by meaning (fuzzy + semantic, auto)
gqls Query.user -R --code ./app       # jump to the graphql-ruby resolver
```

## Why

Nothing else combines fuzzy/semantic search with big-schema speed in a CLI. Schema viewers list and filter but don't fuzzy-match, hosted explorers are GUIs, and the semantic-search tools are built for agents, not developers. `gqls` fills that gap and stays Unix-composable — `-j`/`-J` emit JSON/NDJSON everywhere.

## Install

```sh
# Homebrew (fuzzy + introspection + resolver jump + semantic search)
brew install dpep/tools/gqls

# Cargo (crate is `gqls-cli`; installs the `gqls` binary, semantic search included)
cargo install gqls-cli

# Lean, fuzzy-only build (no ONNX Runtime download)
cargo install gqls-cli --no-default-features
```

The resolver jump (`-R`) shells out to [`rq`](https://github.com/dpep/rq); install it too if you want that.

## Usage

### Input sources
- **SDL file** — `gqls user schema.graphql`
- **Introspection JSON dump** — `gqls user schema.json`
- **Live endpoint** — `gqls user https://api.example.com/graphql` (POSTs the introspection query)
- **Auto-discovery** — omit the source and `gqls` finds a schema in the current tree (preferring `.graphqls`, then `schema.*`, then an introspection `.json`, then any SDL-looking `.graphql`; in a federated monorepo, a `supergraph*` schema wins when several exist).

### Federated schemas (Apollo Federation v2)
`gqls` parses subgraph SDL directly — the `extend schema @link(...)` header and `@key`/`@shareable` directives that trip up plain GraphQL parsers — so you can `cd` into a subgraph package and search its own schema. Auto-discovery follows suit: at the repo root it prefers the composed `supergraph*` schema, but run from inside a subgraph it uses that subgraph's local schema.

### Fuzzy search (default)
Handles abbreviations (`usr` → `User`), typos and transpositions (`usre` → `User`), and qualified `Type.field` queries. Results rank by match quality, with root `Query`/`Mutation` fields floated up.

```sh
gqls createUser -k mutation      # restrict to a kind (plurals ok: mutations)
gqls User.email                  # qualified — boosts the field on User
```

### Semantic search — automatic, combined with fuzzy
By default gqls returns fuzzy matches **and** semantic ones, merged via Reciprocal Rank Fusion, so exact-name and meaning-based hits both surface (fuzzy weighted a touch higher to keep exact matches on top). Semantic ranking uses a local `all-MiniLM-L6-v2` model (ONNX Runtime), fetched once from the HuggingFace Hub then cached offline.

```sh
gqls 'delete a repository'    # a phrase — semantic dominates
gqls user                     # an identifier — fuzzy leads, semantic fills in
gqls user -s                  # force semantic only  (--fuzzy forces fuzzy only)
```

Per-record vectors are cached (keyed by schema content + model). The **first** time gqls sees a schema it returns fuzzy results immediately and embeds the vectors **in the background** — so the next run is combined and instant (GitHub's schema: background embed ~40s once, warm query ~0.3s). Set `GQLS_NO_AUTOWARM=1` to disable that. Editing the schema re-embeds automatically; `--refresh` forces it, `--clear-cache` wipes the cache, and `gqls --warm <schema>` embeds up front (e.g. in CI). Semantic needs a semantic build — the default `cargo install` and the Homebrew build have it; `--no-default-features` is fuzzy-only.

### Resolver jump (`-R`, graphql-ruby)
Find a field, then jump to the resolver or method that implements it, via `rq`:

```sh
$ gqls Query.user schema.graphql -R --code ./app
app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

`gqls` tries graphql-ruby naming conventions (resolver class, type method, mutation class) and ranks the candidates. When the schema is a local file, candidates are also ranked by package proximity to it — so in a federated monorepo the resolver in the schema's own subgraph wins over a same-named one elsewhere.

### Output
Every mode supports `-j`/`--json` (pretty array) and `-J`/`--ndjson` (one record per line). Status chatter goes to stderr, so JSON pipes clean:

```sh
gqls repository schema.json -J | jq -r '.path'
```

Shell completions: `gqls --completions zsh` (or `bash`/`fish`/…).

## Using with Claude Code

Drop a small skill into `~/.claude/skills/gqls/` (see `claude/gqls-skill.md` in this repo) so Claude reaches for `gqls` when navigating a GraphQL schema instead of grepping SDL by hand.

## How it works

Layered so the core is one idea — flatten every schema entity to a searchable record, and let search and output touch nothing but records:

```
src/
  model.rs        SchemaRecord + Kind (the only shared vocabulary)
  load/           SDL parse · introspection (URL/JSON) · schema discovery
  search/         the fuzzy scorer (a DP subsequence aligner + typo tier)
  semantic/       embedding search + on-disk vector cache (feature = "semantic")
  resolve.rs      field -> resolver jump (shells out to rq)
  cli.rs          clap + unified text/json/ndjson output
```

Two capabilities are **borrowed** from sibling tools rather than reinvented: the fuzzy ranking is ported from [`rq`](https://github.com/dpep/rq)'s aligner, and the local embedding pipeline is copied from [`ae`](https://github.com/dpep/ae).

## License

MIT — see [LICENSE](LICENSE).
