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

Several words are one query — `gqls cancel a subscription` needs no quotes —
and the source is recognised wherever it sits among the arguments. A leading
kind word filters like `-k`: `gqls query user`, `gqls type User`. gqls says on
stderr when it read a word that way, and passing `-k` yourself keeps the word
in the query instead.

`<source>` is a `.graphql`/`.graphqls` SDL file, a `.json` introspection dump,
or an `http(s)://…/graphql` URL (introspected live). Omit it and gqls finds a
schema in the current directory tree (`-v` shows which one it picked), falling
back to the enclosing git repo when the directory you're in has none — so you
don't need to `cd` to the repo root first. Apollo Federation v2 subgraph SDL
parses directly, and auto-discovery prefers a composed `supergraph*` schema
when several exist.

Each result is an object:

```json
{ "path": "Query.user", "name": "user", "kind": "query", "parent": "Query",
  "type_ref": "User", "args": ["id: ID!"],
  "description": "Look up a user by id.", "score": 1060 }
```

`path` is the qualified location (`Type.field`), `type_ref` the return/field
type, `args` the argument signatures, `description` the schema doc when the
schema has one — usually enough to confirm a match without opening the schema.
Status lines go to stderr, so `-j`/`--json` and `-J`/`--ndjson` pipe cleanly
into `jq`. A miss prints `gqls: no matches for <q>` to stderr, and means it: semantic
results below a relevance floor are dropped, so a question the schema can't
answer returns nothing rather than its closest noise. Treat an empty result as
"not in this schema", not as "try a different phrasing".

When the query *names* exactly one of its matches — the leaf is that record's
name, not merely its best fuzzy match — gqls stops listing and explains it
instead: the one record, annotated. Searching narrows; naming finds. Case
decides when it's the only thing separating candidates, so `Role` explains the
enum while `role` lists it alongside `User.role`. `--no-explain` forces the list
back.

That record carries three more keys — the signal that you found the thing
rather than a shortlist:

- `match` — `"exact"`, or `"corrected"` when the name was a small misspelling
- `values` — an enum's values, each `{name, description?, deprecated?}`
- `referenced_by` — every path whose type is this one, which is the schema's
  answer to "how do I get one of these"

`deprecated`, `directives` and `possible_types` are ordinary record fields and
appear whenever the schema has them. The array shape never changes, so a reader
that ignores the extra keys still works.

Text output shows the description too — elided to one line in a list, in full
for a record you named. `-D` drops descriptions and collapses an enum's values
to their names.

## Scope when you know more

- Fuzzy / abbreviation / typo: `gqls usr`, `gqls usre`, `gqls createuser`.
- Qualified: `gqls User.email` — when `User` names a schema type (any case,
  misspellings snap to the unique closest type), results are hard-filtered to
  that type's members; otherwise it falls back to fuzzy-matching the whole
  query.
- Wildcard: `gqls User.` lists every field on User — a trailing dot is
  shorthand for `.*` and needs no quoting, so prefer it. The general forms
  are `gqls '*.email'` (that field on every type), `gqls 'get*'` (names
  starting with "get"), `gqls 'User.?d'` (`?` = one character), and
  `gqls 'User.{first,last}Name'` (alternatives) — quote those, since the
  shell would expand `*`/`?`/`{}` first. `*`/`?` span `.`, patterns are
  anchored, and semantic ranking is skipped: this enumerates, it doesn't
  search.
- Return type: `gqls --returns Company` finds fields returning Company even
  when the name doesn't say so (`Query.myEmployer: Company`), ignoring
  `[]`/`!` wrappers; wildcards allowed (`--returns '*Payload'`). Add
  `-k query` to find an entry point into a type. No QUERY needed — it lists
  every match. This is the way to answer "what returns X", which a name
  search cannot.
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
needed. A strong name match — exact, or the word whole at a boundary (`name`
→ `lastName`) — skips the semantic combine (fuzzy found what you typed;
lookalike fields would just pad the list) — `--semantic` forces it back on.
The space form `'User name'` is the loose variant of `User.name`: same type
filter, but semantic stays on so nearby fields (`lastName`) surface too.
Fuzzy matches a phrase word by word (noise words dropped, best coverage
wins), so multi-word queries still return something when the semantic index
is cold or the build is fuzzy-only:

```sh
gqls 'cancel a subscription' <source>     # combined fuzzy + semantic
gqls 'delete a repository' --semantic      # force semantic-only
gqls user --fuzzy                          # force fuzzy-only (skip semantic)
```

Semantic ranking uses a local model (all-MiniLM-L6-v2, ONNX). The first time
gqls sees a schema it returns fuzzy results immediately and embeds the vectors
in the background, so the next run is combined and instant; `gqls --warm
<source>` pre-embeds up front. Editing the schema re-embeds only the records
that changed, so a schema under active development stays cheap. It ships in the default `cargo install` and the
Homebrew build (a `--no-default-features` build is fuzzy-only).

## Many queries at once

Answering a list of questions about one schema? Pipe them on stdin, one per
line, and a single run answers them all — the schema, the embedding model and
the vectors load once instead of per query. 20 meaning-based queries against a
10k-record schema: 1.83s against 0.52s.

```sh
printf 'cancel a subscription\ndispute a transaction\n' | gqls schema.graphql -J
gqls schema.graphql -J < questions.txt
```

Each row carries the `query` that produced it, so one stream stays
attributable, and a query that matched nothing still reports
`{"query": …, "status": "no_matches"}` rather than dropping out. A single
query's output is unchanged, so existing parsing is unaffected. An explicit
query beats a pipe; `-R` and `-e` take one query only.

## Draft a query to paste (`-e`)

When the goal isn't "where is this field" but "give me something I can put in
the code", let gqls build it rather than assembling one by hand:

```sh
gqls Mutation.updateEmployee -e          # operation + variables, as text
gqls Company.employee -e --json          # {path, operation, variables, ...}
gqls Query.user -e --depth 2             # expand one more level of fields
```

Each argument you must supply becomes a variable, with a `"<ID!>"` placeholder
that names its type. Anything the server can supply — nullable, or carrying a
schema default — is left out of the operation and listed underneath, so what
it prints runs as-is. It selects one level of leaf fields, expands an `errors`
block only when the payload really has one, and wraps a nested field in a root
that returns its type. Object-valued fields become `# field: Type { … }`
markers — `--depth N` expands them when you want more. A union is written as
inline fragments over its members (an interface adds one per implementor for
the fields it adds), and deprecated fields stay in the selection
marked `# deprecated: reason` with a stderr warning naming them (tell the user
rather than pasting one silently). Input
objects and enums referenced by the arguments are expanded underneath, so a
`"<SomeInput!>"` placeholder can be filled without a second lookup.

`-e` and `-R` only act on a field the query names outright (or misspells
slightly — `Did you mean X?` on stderr says which, and is worth passing on).
A looser query — `crtusr`, `User.`, a wildcard — answers `Did you mean:`
with the matches and exits nonzero instead; re-run with the path you meant, or
show the user the list if it isn't obvious which one they want.

Two things still need your judgment:

- **Ambiguous entry points.** A `# paths` block under the draft means several
  root fields return that type; the one drafted through is listed first. gqls
  picks the one with the fewest required arguments; in a federated schema
  another path may be the right one. Ask the user rather than silently
  accepting the pick.
- **Unreachable fields.** If it reports no root returns the type, don't invent
  a path — run `gqls --returns <Type>` and show what actually exists.

## Jump to the resolver (graphql-ruby)

Find a field, then jump to the code that implements it, via `rq`:

```sh
gqls Query.user <source> -R --code <server-dir>
# -> app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

Tries graphql-ruby conventions (resolver class, type method, mutation class)
and ranks the candidates, best convention first — a hit only counts as a
convention match if it really sits in the namespace the convention named, so a
model class that merely shares a name can't pose as the resolver. Needs the
`rq` CLI installed and a server dir that's a git repo `rq` has indexed.

Results marked `(guess)` came from a bare name search after every convention
missed — report those as guesses rather than as the resolver. Everything else
is a verified match: mutations, root fields (including a federated subgraph's
own `Query` class), fields with a custom method, and fields declared only as
`field :name, Type`, which resolve to the declaration itself.

That last case needs `rq` 0.35.2 or newer, which indexes `field` declarations
as the methods they define. On an older `rq` those fields silently find
nothing — check `rq --version` before concluding a field has no resolver.

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
- `--profile` reports where a query's time went, as a table on stderr or as
  JSON on stderr alongside `-j`. Reach for it when asked why gqls is slow on a
  schema, rather than guessing.
- `gqls --help` prints the full help with examples.
