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
- **Live endpoint** — `gqls user https://api.example.com/graphql` (POSTs the introspection query; add auth with `-H "Authorization: Bearer …"`, repeatable). Remote responses are cached ~1h so repeat queries don't refetch all day; a `localhost` endpoint is never cached (you're likely editing that schema). Tune with `GQLS_INTROSPECT_TTL` (seconds; `0` disables), `--refresh` to bypass, `--clear-cache` to wipe.
- **Auto-discovery** — omit the source and `gqls` finds a schema in the current tree (preferring `.graphqls`, then `schema.*`, then an introspection `.json`, then any SDL-looking `.graphql`; in a federated monorepo, a `supergraph*` schema wins when several exist). The tree is searched in parallel, hidden directories and known build/dependency directories (`node_modules`, `target`, `venv`, `coverage`, …) are skipped whole, and a candidate is only opened when nothing already found outranks it — so a repo with a `schema.graphql` at a sensible place is found without reading a single file. The answer is then remembered per directory for an hour so repeat runs skip the walk entirely, which is otherwise the most expensive thing a warm query does. If nothing is found beneath you, the search widens rather than failing: first to the enclosing git repository (so `gqls user` in `repo/src/components` finds the repo's schema), then to the generated directories it skipped, so a schema written by a build step is still found. Dependency directories are never searched — a schema in `node_modules` describes someone else's API. A remembered answer is dropped once the schema it names moves; `--refresh` re-walks, `GQLS_DISCOVER_TTL` (seconds; `0` disables) tunes it, and `-v` says which schema answered either way.

### Federated schemas (Apollo Federation v2)
`gqls` parses subgraph SDL directly — the `extend schema @link(...)` header and `@key`/`@shareable` directives that trip up plain GraphQL parsers — so you can `cd` into a subgraph package and search its own schema. Auto-discovery follows suit: at the repo root it prefers the composed `supergraph*` schema, but run from inside a subgraph it uses that subgraph's local schema.

### Fuzzy search (default)

Several words are one query, so `gqls cancel a subscription` needs no quotes, and the schema is recognised wherever it sits among the arguments. A leading kind word filters like `-k` — `gqls query user`, `gqls type User` — and gqls says on stderr when it read a word that way.

Handles abbreviations (`usr` → `User`), typos and transpositions (`usre` → `User`), and qualified `Type.field` queries. Results rank by match quality, with root `Query`/`Mutation` fields floated up. Weak long-tail matches are cut relative to the best hit; `-v` reports the total match count when it exceeds the limit.

```sh
gqls createUser -k mutation      # restrict to a kind (plurals ok: mutations)
gqls User.email                  # qualified — filters to fields on User
gqls 'cancel a subscription'     # a phrase — matched word by word
```

### Name one thing and gqls explains it

Searching narrows; naming finds. When a query names exactly one of the records it matched, that record is shown on its own and annotated — its description in full, its deprecation reason, applied directives, a union's members or an interface's implementors, an enum's values with what each means, an input object's fields with their types, and every path that references the type. A path counts in both directions: a field returning the type, and an argument taking it (`Mutation.createUser(input:)`) — which for an input object, never returned by anything, is the only direction it appears in at all.

```sh
$ gqls Role
gqls: 2 other matches for "Role" (--no-explain to list them)
Role  [enum]
  What a user is allowed to do.

  referenced by  User.role, CreateUserInput.role
  values
    ADMIN   Full access, including billing and member management.
    MEMBER  Ordinary access to the account's own content.
    GUEST   Read-only.
    OWNER   (deprecated: collapsed into ADMIN)
```

An input object gets the same treatment, which is what you need to construct one:

```sh
$ gqls UpdateUserInput
UpdateUserInput  [input_object]
  Fields to change on a user. Every field is optional; omitting one leaves that
  part of the user as it was, which is why nothing here is non-null.

  referenced by  Mutation.updateUser(input:)
  fields
    name     String
    email    String   Re-triggers verification, and fails if another user
                      already has it.
    role     Role
    isAdmin  Boolean  (deprecated: set role: ADMIN instead)
```

Capitalisation decides when it's the only thing separating candidates: `Role` names the enum and not `User.role`, so it explains; `role` names all three and stays a search. `--no-explain` forces the list back, and `-D` collapses an enum's values to their names and empties the description column of an input object's fields. In `--json`/`--ndjson` the record carries `match` (`"exact"` or `"corrected"`) plus `values`, `fields` and `referenced_by`, so a consumer gets the same facts.

### Many queries at once
Pipe queries on stdin, one per line, and a single run answers them all — the schema, the embedding model and the vectors load once instead of once per query. On a 10k-record schema, 20 meaning-based queries drop from 1.83s to 0.52s:

```sh
cat queries.txt | gqls schema.graphql -J
printf 'cancel a subscription\ndispute a transaction\n' | gqls schema.graphql -J
```

Every row carries the `query` that produced it, so one stream stays untangleable, and a query that matched nothing still reports `{"query": …, "status": "no_matches"}` rather than vanishing. A single query's output is unchanged — no `query` field — so existing callers parse exactly what they always did. An explicit query beats a pipe, and `--resolve`/`--example` take one query only.

A multi-word query is matched one word at a time, so a phrase isn't a hard zero when semantic ranking is unavailable or still warming. Noise words (`a`, `the`, `of`, …) are dropped, and the records covering the most words win outright — `cancelSubscription` beats the many that merely echo `subscription`. When nothing covers the whole phrase, every single-word match stands. Only whitespace opens this path: `User.email` is still scored whole, and `User email` becomes the qualified form before the search runs.

### Filter by return type
`--returns TYPE` keeps only fields whose type is `TYPE`, ignoring `[]`/`!` wrappers — the way to find a field when you know what it returns but not what it's called:

```sh
gqls --returns Company                  # every field returning a Company
gqls --returns Company -k query         # ...just the root queries — an entry point
gqls --returns '*Payload'               # wildcards work here too
gqls employee --returns Employee        # combined with a name search
```

A name search can't answer this: `Query.myEmployer: Company` doesn't contain the word "Company" anywhere in its name or path. With no QUERY at all, `--returns` lists everything it matches.

### Wildcards
A wildcard in the query switches from fuzzy search to enumeration — every match is exact, ordered by kind then alphabetically. **Quote the pattern** so your shell doesn't expand it against local filenames:

```sh
gqls User.                     # shorthand for 'User.*' — no quoting needed
gqls 'User.*'                  # every field on User (nested paths included)
gqls '*.email'                 # the email field on every type that has one
gqls 'get*'                    # every name starting with "get"
gqls '*Payment*'               # every name containing "Payment"
gqls 'User.?d'                 # ? matches exactly one character
gqls 'User.{first,last}Name'   # brace alternation, shell-style
gqls '{Query,Mutation}.*'      # every root operation
```

A trailing `.` is shorthand for `.*`, which is the form worth remembering: no shell quoting required. Beyond that, three metacharacters and nothing else: `*` (any run of characters), `?` (exactly one), and `{a,b}` (alternatives, nestable). `*` and `?` span `.`, so `'User.*'` reaches nested paths. Patterns are anchored, so `'User.*'` never wanders into `UserProfile`. There's no escape syntax — GraphQL names can't contain these characters anyway — and a query with whitespace is treated as prose, so a phrase ending in `?` stays a normal search.

Wildcards skip semantic ranking (you asked for a list, not a guess); combine them with `-k` to narrow further (`gqls '*.email' -k input_field`).

In a qualified query, a `Type` that names a schema type (any case) becomes a hard filter — `Company.employe` searches only `Company`'s members, not every type starting with "Company". A misspelled qualifier snaps to the unique closest type (`Compnay.employe` → `Company`, announced on stderr); one that matches nothing falls back to plain fuzzy matching. That correction applies to fuzzy queries, not to wildcards — patterns match literally, so `Compnay.` finds nothing rather than guessing.

### Semantic search — automatic, combined with fuzzy
By default gqls returns fuzzy matches and semantic ones, merged via Reciprocal Rank Fusion, so exact-name and meaning-based hits both surface (fuzzy weighted a touch higher to keep exact matches on top). Semantic ranking uses a local `all-MiniLM-L6-v2` model (ONNX Runtime), truncated to 64 dimensions and cosine-ranked; the model is fetched once from the HuggingFace Hub, then cached offline. What gets embedded is the record's path, description and return type, with identifiers split into words (`cancelSubscription` → `cancel Subscription`) and `[]`/`!` wrappers dropped — the tokenizer shreds camelCase into meaningless word pieces otherwise.

```sh
gqls 'delete a repository'    # a phrase — semantic leads, fuzzy matches per word
gqls usr                      # an identifier — fuzzy leads, semantic fills in
gqls user --semantic          # force semantic only  (--fuzzy forces fuzzy only)
```

When the query names something that exists — an exact match, or the word whole at a boundary (`name` → `lastName`) — the combine is skipped: fuzzy found what you typed, so meaning-based lookalikes would only pad the list. `--semantic` forces it back on. The space form (`gqls 'User name'`) is the loose variant: same type filter, but semantic stays on, so nearby fields (`lastName`, `firstName`) surface too. Semantic results are tail-bounded relative to the best hit, so a large `-l` can't fill with monotonic noise.

Per-record vectors are cached, keyed by schema content and model. The first time gqls sees a schema it returns fuzzy results immediately and embeds in the background — so the next run is combined and instant (GitHub's schema: ~40s to embed once, then ~0.3s warm queries). `GQLS_NO_AUTOWARM=1` disables the background embed. Editing the schema re-embeds only what changed — vectors are keyed per record, so adding a few fields costs a few inferences, not the whole schema — and the superseded cache file is collected, so a drifting schema keeps one file rather than one per edit. `--refresh` forces a full re-embed, `--clear-cache` wipes the cache, and `gqls --warm <schema>` embeds up front (e.g. in CI). Semantic needs a semantic build — the default `cargo install` and Homebrew have it; `--no-default-features` is fuzzy-only.

### Draft an example operation (`-e`)
Find a field, then get something you can paste into a client:

```sh
$ gqls Mutation.updateEmployee -e
mutation UpdateEmployee($companyId: ID!, $input: EmployeeInput!) {
  updateEmployee(companyId: $companyId, input: $input) {
    errors {
      message
      path
    }
    clientMutationId
    # employee: Employee { … }
  }
}

# optional arguments:
#   updateEmployee(dryRun: Boolean = false)

# enums:
#   Role = ADMIN | MEMBER | GUEST

# variables — input: EmployeeInput!
{
  "companyId": "<ID!>",
  "input": {
    "name": "<String!>",
    "role": "<Role>"
  }
}
```

`-e` and `-R` act only on a field the query actually names — the name itself, or a small misspelling of it (`createUesr`, which says `Did you mean Mutation.createUser?` above the answer). Anything looser (`crtusr`, `User.`, a wildcard) answers `Did you mean:` with the matches it found and exits nonzero: search is happy to rank the closest of what's there, but a drafted operation or a file:line both read as authoritative, so guessing which field was meant is worse than asking.

The rules are deliberately conservative, because a wrong guess costs more than a visible hole:

- **Arguments you must supply become variables** — nothing is inlined into the query body, and each placeholder names its type (`"<ID!>"`), so it can't be mistaken for a usable value the way `""` or `0` can.
- **Anything the server can supply is left out and listed underneath** — a nullable argument, or one with a schema default (even a non-null one, like `first: Int! = 10`). The operation runs as-is, and the knobs you skipped are still visible with their defaults.
- **One level of selection, leaf fields only.** A scalar or enum return gets no selection set at all. An object return gets its scalar/enum fields plus a `# field: Type { … }` marker per object-valued field.
- **An `errors` block only if the schema really has one** — the payload/errors convention is common, not universal, so it's expanded only when that field exists.
- **The variables block is a fillable skeleton, not a restatement.** An input-object argument is expanded into its fields in the schema's own order, so the thing you paste into a client is the thing that shows the shape — `"<EmployeeInput!>"` only ever repeated the signature twenty lines above it. A list gets one element; a self-reference (`Filter { and: [Filter!] }`) closes as `"<Filter!>"`, since its shape is in the object directly containing it. An input the schema has no fields for stays a placeholder rather than becoming a `{}` that claims it takes nothing. Since an expanded key no longer names its type, the heading does: `# variables — input: EmployeeInput!`.
- **Enums are listed beside the block, not inside it** — JSON has no way to hold "one of these", so `# enums:` names the choice for each enum the variables reach. It's the only thing left that the skeleton can't express.
- **Abstract types become inline fragments.** A union has no fields of its own, so it's written as `... on Member { … }` over each concrete type — the only form a server accepts. An interface selects its common fields once, then adds a fragment per implementor carrying only the fields that implementor *adds*, since those are otherwise unreachable. Big unions list the first few and name the rest.
- **`--depth N` selects more levels**, expanding the object-valued fields that depth 1 leaves as markers.
- **Deprecated fields are flagged, not dropped.** They stay in the selection marked `# deprecated: reason`, and a stderr line names them — silently omitting a field the schema still serves is its own surprise.
- **A nested field is wrapped in a root that returns its type.** `gqls Company.employee -e` finds a root returning `Company` (preferring one with fewest required arguments) and nests through it. When several roots qualify, a `# paths` block lists them all with the drafted one first. When none does, it's an error with a pointer to `--returns`, not a guess.
- **An input object is drafted through the field that takes it.** An input is never callable, but it is always passable, so `gqls PostFilter -e` drafts the operation whose argument it is — `Query.posts(filter:)`, listed in `# paths` alongside any other field taking one. Naming an input field (`CreateUserInput.email`) drafts through its enclosing input, the thing an operation can actually name. The argument carrying it is supplied even where the schema calls it optional: a draft for `PostFilter` that quietly leaves `filter` out answers nothing. When no field takes one, that's an error rather than a guess.
- **An input draft stays about the input.** It asks where the value goes, not what comes back, so the reply gets the barest selection a server accepts (`__typename`) and only the named input's own types are expanded — `gqls PostFilter -e` was spelling out `PostOrder`, `PostOrderField` and `OrderDirection` off a sibling argument it doesn't even fill in. `--depth 1` asks for the payload back. (`--depth 0` means the same barest selection for any target; it used to be silently clamped to 1.)
- **Each section appears only when it has something in it** — no empty `# variables` block for an operation that takes none, and no `# paths` block when there's only the one the draft already shows.

Every drafted operation is parsed back with a GraphQL parser in the test suite, and a network-gated test drafts against a live endpoint and *executes* the result there — so what it prints is not just well-formed but accepted by the server it came from. `-j`/`--json` emits `{path, operation, variables, optional_args, enums, variable_types, deprecated, paths}` for scripting.

### Resolver jump (`-R`, graphql-ruby)
Find a field, then jump to the resolver or method that implements it, via `rq`:

```sh
$ gqls Query.user schema.graphql -R --code ./app
app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

Like `-e`, it only resolves a field the query names (or misspells slightly) — a looser query gets the candidate list and a nonzero exit instead, before `rq` is ever consulted.

`gqls` tries graphql-ruby naming conventions (resolver class, type method, mutation class) and ranks the candidates, best convention first; package proximity to the schema file breaks ties, so in a federated monorepo the resolver in the schema's own subgraph wins over a same-named one elsewhere.

**Its reliability varies by field kind, and it says so.** Mutations are the strong case: `Mutations::VerbNoun` is a near-universal convention. Root fields also try the bare root class (`Query#field`), since a federated subgraph names its root `Query` rather than `QueryType`. When no convention matches, gqls falls back to searching for the name alone — those results are marked `(guess)` and announced on stderr, because a name-similarity hit is not a resolver lookup. Fields declared purely as `field :name, Type` with no method body currently can't be found at all: `rq` locates definitions, and a macro call isn't one.

### Output
Text results carry the path, the type, the kind, and the schema description — a match is usually confirmable without opening the schema:

```sh
$ gqls user examples/schema.graphql
Query.user(…)            -> User      [query]   — Look up a user by id.
User                                  [object]  — An account.
ArchiveUserPayload.user  -> User      [field]
Query.users(…)           -> [User!]!  [query]   — Page through every user in…
UserError                             [object]  — Something the caller can fix,…
```

An argument list collapses to `(…)` beside other results and spells out in full
when a result stands alone — the list is for telling rows apart, and one long
signature would set the column width for every one of them.

In a list a description is elided to one line — enough to tell one row from the next, and dropped when the columns leave no room for even that. A result shown on its own gets the whole thing, wrapped to your terminal. `-D`/`--no-description` drops descriptions, collapses an enum's values to their names, and empties the description column of an input object's fields. Every mode also supports `-j`/`--json` (pretty array) and `-J`/`--ndjson` (one record per line), which always carry the full description text. Status chatter goes to stderr, so JSON pipes clean:

```sh
gqls repository schema.json -J | jq -r '.path'
```

`-q`/`--quiet` silences the stderr status lines (results and hard errors still print); `-v`/`--verbose` adds diagnostics — cache hits/misses, the `rq` candidates `-R` tried, and why the embedding model loaded or fell back to the hash embedder. Under `-R`, verbose also passes `-v` through to `rq` and streams its trace.

`--profile` prints a phase-by-phase breakdown to stderr — where a query's time actually goes, with counts alongside the timings:

```sh
$ gqls user big.graphql --fuzzy --profile
  load          9.4ms  48501 records
  fuzzy scan    6.5ms  2464 of 48501 records matched
  output        0.0ms
  ──────────
  total        16.3ms
```

The phases have to add up. When they don't, the report says so on an `unaccounted` line rather than leaving you to subtract — un-instrumented work is exactly what a profile is for, and a report that quietly omits it points you at the phases that are fast instead of the seconds that aren't. Nested phases (`cache: read` inside `load`) are shown but not double-counted.

With `-j`/`-J` the same data goes to stderr as JSON — including `unaccounted_ms` and each phase's nesting `depth` — so stdout stays exactly the results and a baseline can be stored and diffed. Profiling costs nothing when off: a disabled span reads no clock, takes no lock and allocates nothing, which measures as no difference across 30 runs.

`script/bench.sh` runs a fixed query set against a generated 48k-record schema and prints medians per phase; `--save NAME` stores a baseline and `--diff NAME` compares against it, so a change's effect is a diff rather than a memory.

Shell completions: `gqls --completions zsh` (or `bash`/`fish`/…).

## Using with Claude Code

`gqls` ships with a Claude Code skill (`claude/gqls-skill.md`) so Claude reaches for it when navigating a GraphQL schema instead of grepping SDL by hand. Two ways to install it — the marketplace plugin, which updates itself and brings the sibling skills, or a local copy of the one file:

```
/plugin marketplace add dpep/claude
/plugin install code@dpep
```

```sh
mkdir -p ~/.claude/skills/gqls
cp claude/gqls-skill.md ~/.claude/skills/gqls/SKILL.md
```

The plugin is the better default; [`claude/INSTALL.md`](claude/INSTALL.md) covers when it isn't, and the binary install either route still needs.

## Development

`script/check.sh` is the gate — formatting, clippy, and tests across every feature configuration gqls ships (default/semantic, fuzzy-only, and the `semantic-dynamic` build Homebrew uses). Run it before pushing.

`script/release.sh <version | major | minor | patch>` cuts a release: bump, changelog heading, gate, commit, tag, push, `cargo publish`, Homebrew formula (tarball sha, build, test, audit, tap push), the GitHub release page from the changelog section, and the plugin skill copy. `--dry-run` prints the plan without touching anything, and `--summary "…"` sets the commit subject and release title. Every step checks whether it has already happened, so a run interrupted at step twelve is re-run with the same arguments and resumes.

## How it works

Layered so the core is one idea — flatten every schema entity to a searchable record, and let search and output touch nothing but records:

```
src/
  model.rs        SchemaRecord + Kind (the only shared vocabulary)
  load/           SDL parse · introspection (URL/JSON) · schema discovery
  search/         the fuzzy scorer (a DP subsequence aligner + typo tier)
  semantic/       embedding search + on-disk vector cache (feature = "semantic")
  example.rs      operation drafting (-e)
  style.rs        ANSI weights + the column layout for text output
  resolve.rs      field -> resolver jump (shells out to rq)
  cli.rs          clap + unified text/json/ndjson output
```

Two capabilities are borrowed from sibling tools rather than reinvented: the fuzzy ranking is ported from [`rq`](https://github.com/dpep/rq)'s aligner, and the local embedding pipeline is copied from [`ae`](https://github.com/dpep/ae).

## License

MIT — see [LICENSE](LICENSE).
