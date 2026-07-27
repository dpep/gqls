# gqls — GraphQL schema search

Fuzzy and semantic search over the types, fields, args, and directives in a
GraphQL schema — from the terminal, for very large graphs.

```sh
gqls user examples/schema.graphql          # fuzzy search
gqls cu examples/schema.graphql            # abbreviations: `cu` -> createUser
gqls email examples/schema.graphql -k field # restrict to a kind
gqls user examples/schema.graphql --json    # machine-readable
gqls repository schema.json                  # local introspection JSON dump
gqls repository https://api.example.com/graphql  # live introspection
gqls user                                    # no path -> auto-discover a schema
```

Input is a local `.graphql`/`.graphqls` SDL file, a local introspection JSON
dump (`*.json`), or an http(s) URL (introspected on the fly). With no source
argument, gqls walks the current directory tree for a schema document
(preferring `.graphqls`, then `schema.*`, then an introspection `.json`, then
any SDL-looking `.graphql`).

## Semantic search

Behind the `semantic` feature — meaning-based, not just fuzzy:

```sh
cargo build --features semantic
gqls "which mutation cancels a subscription" --semantic examples/schema.graphql
```

It embeds each record (`path + description + type`) and the query with a local
`all-MiniLM-L6-v2` model via ONNX Runtime, compresses to 64-d Matryoshka
vectors, and ranks by cosine. The model is fetched from the HuggingFace Hub on
first run, then cached offline; if it can't be fetched it falls back to a
deterministic hash embedder so search still runs.

## Why this exists

Existing tools don't combine fuzzy/semantic search with big-schema speed from a
CLI: `gquil` lists and filters but doesn't fuzzy-match; Apollo GraphOS has
search but it's a hosted GUI; MCP servers do semantic schema search but for
agents, not developers. `gqls` fills that gap and stays Unix-composable.

## Design

Layered like [`rq`](https://github.com/dpep/rq): a **loader** turns a schema
into flat `SchemaRecord`s, and a **search** layer ranks them. Nothing below the
loader knows about GraphQL syntax or transports.

```
src/
  model.rs           SchemaRecord + Kind (the only shared vocabulary)
  load/
    mod.rs           load(source) + discover() for the no-arg case
    sdl.rs           parse SDL -> records
    introspect.rs    URL / JSON introspection -> records
  search/
    mod.rs           filter -> score -> rank -> truncate
    score.rs         fuzzy scorer (shape borrowed from rq)
  semantic/          embedding search (feature = "semantic")
    mod.rs           embed records + query, rank by cosine
    cache.rs         on-disk per-record vector cache (schema+embedder keyed)
    embed.rs         Embedder trait + HashEmbedder fallback   (borrowed: ae)
    embed/onnx.rs    all-MiniLM-L6-v2 via ONNX Runtime         (borrowed: ae)
    mrl.rs           Matryoshka truncation + cosine            (borrowed: ae)
  cli.rs             clap + text/json/ndjson output
```

### Borrowed, not rebuilt

- **Fuzzy ranking** — `search/score.rs` ports `rq`'s DP subsequence aligner
  (boundary/contiguity scoring, adjacent-word rule, gap penalties), adapted to
  `SchemaRecord` with a `Type.field` qualifier/parent boost.
- **Semantic search** — `semantic/{embed.rs,embed/onnx.rs,mrl.rs}` are copied
  from `ae` (`~/code/lib/rust/ae`), whose embedding pipeline is generic over
  `&str`. We copy those files; we do **not** depend on `ae`, whose store/CLI
  only speak "acronyms".

### Planned: field → resolver jump (graphql-ruby)

"Open the resolver for this field" becomes a *handoff to `rq`* — find the field
here, then `rq Type#field` locates the graphql-ruby definition in code. Schema
tool owns the schema; rq owns the code.

## Status

Working: SDL / JSON-dump / URL loading, no-arg discovery, the rq-derived fuzzy
scorer, semantic search with an on-disk embedding cache, and text/JSON/ndjson
output. The resolver-jump handoff to `rq` is the main remaining idea.
