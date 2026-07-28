---
name: gqls
description: Search a GraphQL schema — find a type, field, argument, or directive by fuzzy name or by meaning, or jump to a field's graphql-ruby resolver — with the `gqls` CLI. Use for "where is the X type/field", "what mutation does Y", "what returns Z", or navigating a large schema (an SDL file, an introspection JSON dump, or a live endpoint). Prefer over grep/rg for schema lookups — it ranks the intended match first and handles camelCase/snake_case/typos. Not for raw text search.
---

# gqls — search a GraphQL schema

`gqls` is a GraphQL schema *navigation* engine: give it a name or a natural-
language phrase and it returns the ranked match — a type, field, argument, enum
value, or directive — not every textual hit. Reach for it whenever the question
is "where is this in the schema?" Use `grep`/`rg` for raw text; gqls for the
schema.

## Use it like this

Ask for JSON so you can act on the result:

```sh
gqls <query> <source> --json
```

`<source>` is a `.graphql`/`.graphqls` SDL file, a `.json` introspection dump,
or an `http(s)://…/graphql` URL (introspected live). Omit it and gqls finds a
schema in the current directory tree (`-v` shows which one it picked). Apollo
Federation v2 subgraph SDL parses directly, and auto-discovery prefers a
composed `supergraph*` schema when several exist.

Each result is an object:

```json
{ "path": "Query.user", "name": "user", "kind": "query", "parent": "Query",
  "type_ref": "User", "args": ["id: ID!"], "score": 1060 }
```

`path` is the qualified location (`Type.field`), `type_ref` the return/field
type, `args` the argument signatures — usually enough to confirm a match without
opening the schema. Status lines go to stderr, so `-j`/`--json` and `-J`/`--ndjson`
pipe cleanly into `jq`. A miss prints `gqls: no matches for <q>` to stderr.

## Scope when you know more

- Fuzzy / abbreviation / typo: `gqls usr`, `gqls usre`, `gqls createuser`.
- Qualified: `gqls User.email` — when `User` names a schema type (any case,
  misspellings snap to the unique closest type), results are hard-filtered to
  that type's members; otherwise it falls back to fuzzy-matching the whole
  query.
- Kind: `gqls createUser -k mutation` — object, field, query, mutation, enum,
  scalar, input_object, interface, union, directive (plurals ok). A bad kind
  lists the valid ones.
- Count: `-l 1` for just the top hit, larger to survey (default 20). Weak
  long-tail matches are dropped relative to the best hit; `-v` reports the
  total match count when it exceeds the limit.

```sh
gqls repository schema.json -k object -l 5 --json
gqls user https://api.example.com/graphql --json     # live introspection
gqls user https://api/graphql -H "Authorization: Bearer $TOKEN" --json   # auth'd
```

For a live endpoint, add auth with `-H "Name: Value"` (repeatable). Remote
responses are cached ~1h (`localhost` is never cached); `--refresh` forces a
fresh fetch, `--clear-cache` wipes the lot.

## Semantic search (automatic)

gqls combines fuzzy and semantic ranking by default — meaning-based matches
surface alongside name matches, so "what does X" phrases just work, no flag
needed. An exact name match skips the semantic combine (you named the entity;
lookalike fields would just pad the list) — `--semantic` forces it back on.
The space form `'User name'` is the loose variant of `User.name`: same type
filter, but semantic stays on so nearby fields (`lastName`) surface too:

```sh
gqls 'cancel a subscription' <source>     # combined fuzzy + semantic
gqls 'delete a repository' --semantic      # force semantic-only
gqls user --fuzzy                          # force fuzzy-only (skip semantic)
```

Semantic ranking uses a local model (all-MiniLM-L6-v2, ONNX). The first time
gqls sees a schema it returns fuzzy results immediately and embeds the vectors
in the background, so the next run is combined and instant; `gqls --warm
<source>` pre-embeds up front. It ships in the default `cargo install` and the
Homebrew build (a `--no-default-features` build is fuzzy-only).

## Jump to the resolver (graphql-ruby)

Find a field, then jump to the code that implements it, via `rq`:

```sh
gqls Query.user <source> -R --code <server-dir>
# -> app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

Tries graphql-ruby conventions (resolver class, type method, mutation class) and
ranks the candidates. Needs the `rq` CLI installed and a server dir that's a git
repo `rq` has indexed.

## Installing / updating the binary

If `gqls` isn't on PATH, install it, then retry:

```sh
brew install dpep/tools/gqls    # macOS/Homebrew — includes semantic search
```

No Homebrew?

```sh
cargo install gqls-cli                          # semantic search included
cargo install gqls-cli --no-default-features    # lean, fuzzy-only
```

To update: `brew upgrade dpep/tools/gqls` (or re-run the `cargo install` line).
Source + issues: <https://github.com/dpep/gqls>.

## Notes

- The resolver jump (`-R`) is graphql-ruby-specific and shells out to `rq`.
- `-v`/`--verbose` shows diagnostics (cache hits, rq candidates, why the model
  loaded or fell back); `-q`/`--quiet` silences the stderr status lines.
- `gqls -h` prints full help with examples.

To install this skill for Claude Code, copy it to `~/.claude/skills/gqls/SKILL.md`.
