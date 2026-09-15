---
name: gqls
description: Search a GraphQL schema, and draft operations against it, with the `gqls` CLI. Use for "where is the X type/field", "what mutation does Y", "what returns Z", "what fields does Z have" (`gqls User`), or finding a record by meaning rather than name ("cancel a subscription"); `--example` drafts a query or mutation to paste, and `--resolve` jumps to a field's graphql-ruby resolver. Works against an SDL file, an introspection JSON dump, or a live endpoint. Prefer over grep/rg for anything schema-shaped — it ranks the intended match first and sees through camelCase/snake_case and typos (spelling, not vocabulary: a synonym or a dropped word needs a phrase). Not for raw text search.
---

# gqls — search a GraphQL schema

`gqls` is a GraphQL schema *navigation* engine: give it a name or a natural-
language phrase and it returns the ranked match — a type, field, argument, enum
value, or directive — not every textual hit. Reach for it whenever the question
is "where is this in the schema?" Use `grep`/`rg` for raw text; gqls for the
schema.

This file is the agent-facing guide: what to run, what comes back, and where
the tool will mislead you if you take it at its word. The repo's README covers
the same tool for a human reader. You don't need both.

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
when several exist — but parsing is all it does. `@join__*` comes back as raw
text; gqls doesn't interpret federation semantics (ownership, `@key`,
`@requires`, `@external`). It does now *print* them: naming a type lists each of
its fields with the directives applied to it, so `gqls Product` against a
supergraph shows `@join__field(graph: PRODUCTS)` per field in one command. Quote
that and say it's the supergraph's own text — reading ownership off it is fine,
inferring anything it doesn't say is not.

Each result is an object:

```json
{ "path": "Query.user", "name": "user", "kind": "query", "parent": "Query",
  "type_ref": "User", "args": ["id: ID!"],
  "description": "Look up a user by id.", "score": 1000.0 }
```

`path` is the qualified location (`Type.field`), `type_ref` the return/field
type, `args` the argument signatures, `description` the schema doc when the
schema has one — usually enough to confirm a match without opening the schema.

**`score` orders results within one query and nothing else. Never threshold on
it.** Three different rankers write that key and they don't share a scale: a
fuzzy match is the fraction of a perfect one times 1000, so 1000 means the query
*is* the name (plus up to 300 more when a `Type.` qualifier names the right
parent); a `--semantic` run reports a cosine in 0..1; and the default combine of
the two reports a rank-fusion score around 0.03. It is `null` when a record was
explained without ranking having scored it — the key is always present. Read the
order, not the number.

That scale changed in 0.24.0, and the old one topped out at 1060. If you are
resuming work that compared scores against a constant, the constant is wrong.

Status lines go to stderr, so `-j`/`--json` and `-J`/`--ndjson` pipe cleanly
into `jq`. A miss means it: semantic results below a relevance floor are
dropped, so a question the schema can't answer returns nothing rather than its
closest noise. The message says what made it a miss — the filters in play and
what dropping them would find (`nothing returns Issue with -k query — 43 match
without it`), and the schema when gqls discovered one rather than being handed
it (`no matches for "country" in examples/schema.graphql`). Read that last part
before retrying: a miss against a schema you didn't choose is often the wrong
schema, not an absent field.

**Exit codes say which kind of nothing you got.** `0` means gqls answered, and
an empty result is an answer — a miss exits `0`, so don't read a zero exit as
"found something", and don't read a miss as a tool failure worth retrying
differently. `1` means a mode couldn't deliver what it promises: `-e`/`-R` given
a query that names no one record, a schema that won't load, a kind that isn't a
kind. `2` is a usage error the argument parser rejected — you got the flags
wrong, fix the command. Check the code before parsing stdout.

When the query *names* exactly one of its matches — the leaf is that record's
name, not merely its best fuzzy match — gqls stops listing and explains it
instead: the one record, annotated. Searching narrows; naming finds. Case
decides when it's the only thing separating candidates, so `Role` explains the
enum while `role` lists it alongside `User.role`. `--no-explain` forces the list
back.

That record carries five more keys — the signal that you found the thing
rather than a shortlist:

- `match` — `"exact"`, or `"corrected"` when the name was a small misspelling
- `values` — an enum's values, each `{name, description?, deprecated?}`
- `fields` — the fields of an object, an interface or an input object, each
  `{name, args?, type, default?, description?, deprecated?, directives?}`. The
  `!` in `type` says which must be supplied — unless there's a `default`, which
  is what makes even a non-null field optional (`direction: OrderDirection! =
  ASC` may be omitted). Never elided in JSON; the text form stops at a couple of
  dozen and prints the command that lists the rest
- `arguments` — a field's arguments with what each is *for*, when the schema
  documents any of them, each `{name, type, default?, description?}`. The
  signature says what to pass; this is the half it can't carry, and it's the
  first thing to check before guessing what a required argument wants
- `referenced_by` — every path whose type is this one, which is the schema's
  answer to "how do I get one of these". Both directions: a field returning the
  type, and an argument taking it (`Mutation.createUser(input:)`). An input
  object is never returned, so the second is the only way it appears at all.

`deprecated`, `directives` and `possible_types` are ordinary record fields and
appear whenever the schema has them. The array shape never changes, so a reader
that ignores the extra keys still works.

`directives` carries one caveat: introspection exposes directive *definitions*
but not their applications, so it is always empty from a live endpoint or a JSON
dump however many the SDL applies. `@deprecated` is the exception — it arrives
by its own channel and is reconstructed. If a user asks what's applied to a
field, you need the SDL.

`possible_types` carries another: an abstract type's members come back in the
order the source gives them — document order from SDL, the server's own from an
introspection dump — so the same schema read two ways can list them
differently. Membership is the guarantee; order is not, so never present the
first member as primary or the order as meaningful.

**Explaining is triggered by the letters, not by what you meant.** A coincidental
exact match wins and is then reported in full, which reads as authority: against
one real schema `gqls 'update address'` explains the enum value
`SupportTicketDispositionLink.UPDATE_ADDRESS` while
`UserMutation.update_user_address` sits in the matches it didn't show. The
`N other matches` line above the answer is the tell — when the record you got
isn't the one the user asked about, re-run with `--no-explain` and read the list
before reporting anything.

Text output shows the description too — elided to one line in a list, in full
for a record you named. `-D` drops descriptions, collapses an enum's values to
their names, and empties the description column of a type's fields.

**Naming a type is how you list its fields** — `gqls User`, not `gqls User.`.
The wildcard form is a ranked search that stops at `-l` and can drop fields
without the answer looking incomplete; naming the type lists them in schema
order with types, descriptions and deprecations. Keep `User.` for when you want
to *search* within a type (`gqls 'User.*email*'`).

On a toy schema the two look identical, which is how the habit goes wrong. The
difference bites on a real one: `gqls 'Repository.'` returns 20 of GitHub's 145
fields, alphabetical, descriptions clipped — and 20 fields is a perfectly
plausible-looking type. Never answer "what fields does X have" from the wildcard
form.

## Scope when you know more

- Fuzzy / abbreviation / typo: `gqls usr`, `gqls usre`, `gqls createuser`.
  This bridges **spelling, not vocabulary** — it matches names built from your
  query's characters in order. A synonym or a dropped domain word is a different
  name, not a mangled one: `currentUser` finds nothing when the field is `me`,
  and `updateAddress` misses `updateUserAddress` once an unrelated
  `UPDATE_ADDRESS` outranks it. When you're guessing at a name rather than
  quoting one the user gave you, search the word you're sure of (`address`) or
  write a phrase, which is what turns semantic ranking on.
- Qualified: `gqls User.email` — when `User` names a schema type (any case,
  misspellings snap to the unique closest type), results are hard-filtered to
  that type's members; otherwise it falls back to fuzzy-matching the whole
  query. Members includes enum values, so `gqls join__Graph.PRODUCTS` is how you
  look up one value and its directives.
- Two bare words become that qualified form when the first names a type, and
  the rewrite has **no fallback**: `gqls Post role` says
  `no matches for "Post.role"` on stderr, though `gqls role` alone finds four
  records. It does not stop there — semantic ranking still answers within the
  type filter, so stdout carries `Post.id` and `Post.author`, neither of which
  is the field you asked for. **Check stderr before trusting these rows**: a
  `no matches` line above a non-empty stdout means everything below it is
  meaning-based filler. Drop the type word and retry. Quoting doesn't help.
- Wildcard: `gqls User.` lists every field on User — a trailing dot is
  shorthand for `.*` and needs no quoting, so prefer it. The general forms
  are `gqls '*.email'` (that field on every type), `gqls 'get*'` (names
  starting with "get"), `gqls 'User.?d'` (`?` = one character), and
  `gqls 'User.{first,last}Name'` (alternatives) — quote those, since the
  shell would expand `*`/`?`/`{}` first. `*`/`?` span `.`, patterns are
  anchored, and semantic ranking is skipped: this enumerates, it doesn't
  search.
- Argument name: `gqls followRenames` finds the field that *takes* that
  argument — an argument is not a record of its own, so the field is the
  answer, and naming it then shows what the argument is for. It's a second
  pass, run only when names and paths matched nothing at all.
- Return type: `gqls --returns Company` finds fields returning Company even
  when the name doesn't say so (`Query.myEmployer: Company`), ignoring
  `[]`/`!` wrappers; wildcards allowed (`--returns '*Payload'`). Add
  `-k query` to find an entry point into a type. When nothing returns the type
  outright — an interface, usually — the filter widens to what does reach it
  and says so on stderr, rather than answering nothing. No QUERY needed — it lists
  every match. This is the way to answer "what returns X", which a name
  search cannot.
- **DB-generated schema (Hasura, PostGraphile): name the type or use `-k`.**
  Every name shares a prefix — `pokemon_v2_pokemon`, `pokemon_v2_item`,
  `pokemon_v2_move` — so one bare word matches the entire table list. Ranking
  puts the table you meant first, but the rest follows it closely enough that a
  `-l 1` read is a coin flip on which table you report. Spell the type out, or
  add `-k query` for entry points only.
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
fresh fetch, `--clear-cache` wipes the lot. The cache key covers the headers as
well as the URL, so changing a token fetches a fresh schema rather than
replaying the one the previous credentials got — a cache hit is never evidence
that the token you just passed works.

## Semantic search (automatic)

gqls combines fuzzy and semantic ranking by default — meaning-based matches
surface alongside name matches, so "what does X" phrases just work, no flag
needed. A strong name match — exact, or the word whole at a boundary (`name`
→ `lastName`) — skips the semantic combine (fuzzy found what you typed;
lookalike fields would just pad the list) — `--semantic` forces it back on.
So a query that names a real field or type never touches the semantic path.
That's the right answer, but it means semantic ranking only shows up when you
write a phrase, and `-v` is what tells you it was skipped.
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

**Write the phrase as search terms, not as a question.** Noise words are dropped
before scoring, but everything else you type is a word some record can cover, so
a full sentence pulls the ranking toward whatever echoes its incidentals:
`user credit score` finds the field that `where do we expose a users credit
score` buries. Trim a user's question to its content words before passing it on.

Semantic ranking uses a local model (all-MiniLM-L6-v2, ONNX). The first time
gqls sees a schema it returns fuzzy results immediately and embeds the vectors
in the background, so the next run is combined and instant; `gqls --warm
<source>` pre-embeds up front. That is minutes on a large schema, and how many
depends on the machine and the build, so don't promise the user a duration —
gqls prints its own estimate from the rate the run is achieving
(`embedded 8076/11217 (~41s left)`), to a pipe every 15 seconds. Quote that line
rather than guessing. Editing the schema re-embeds only the records that
changed, so a schema under active development stays cheap. It ships in the
default `cargo install` and the Homebrew build (a `--no-default-features` build
is fuzzy-only).

If the model can't be loaded, gqls falls back to a hash embedder and every
`--json`/`--ndjson` row carries **`degraded: true`** (the key is absent
otherwise). Those results are much weaker than they look. Say so rather than
reporting them as findings.

## Many queries at once

**If you have more than one question about a schema, batch them.** Pipe the
queries on stdin, one per line, and a single run answers them all — the schema,
the embedding model and the vectors load once instead of once per query. Against
GitHub's 11,217-record schema, 20 meaning-based queries take 2.7s one at a time
and 0.7s batched: **about 4x**, and it is the largest single lever you have over
how long a schema investigation takes. Gather the questions first and ask them
together, rather than running gqls once per thought.

```sh
printf 'cancel a subscription\ndispute a transaction\n' | gqls schema.graphql -J
gqls schema.graphql -J < questions.txt
```

**A batch takes `-J`, never `-j`.** `-j` is one complete array per query, which
concatenated is nothing a parser reads, so a batch refuses it and says so.

Each row carries the `query` that produced it, so one stream stays
attributable, and a query that matched nothing still reports
`{"query": …, "status": "no_matches"}` rather than dropping out. A single
query's output is unchanged, so existing parsing is unaffected. A piped query
that names one record explains it, the same as one typed as an argument —
so a batch is a way to ask for several explanations at once, not a weaker
mode. An explicit query beats a pipe; `-R` and `-e` take one query only.

## Draft a query to paste (`-e`)

When the goal isn't "where is this field" but "give me something I can put in
the code", let gqls build it rather than assembling one by hand:

```sh
gqls Mutation.createUser -e              # operation + variables, as text
gqls Company.employee -e --json          # {path, operation, variables, ...}
gqls Query.user -e --depth 2             # expand one more level of fields
```

Each argument you must supply becomes a variable, with a `"<ID!>"` placeholder
that names its type. Anything the server can supply — nullable, or carrying a
schema default — is left out of the operation and listed underneath, so what
it prints runs as-is. An `# arguments:` block carries what the schema says each
argument is for, when it says anything. It selects one level of leaf fields,
expands an `errors` block only when the payload really has one, and nests a
field through the chain of fields that reaches it — one hop where a root
returns its type, and as many as it takes where a schema namespaces its roots
(`Query.payroll: PayrollQueries`, with the real fields hanging off that), up to
six. The chain is reported as `Query.payroll > PayrollQueries.company`; past six
hops, and for a type nothing reaches, `-e` says so and names the distance rather
than guessing. Object-valued fields become `# field: Type { … }`
markers — `--depth N` expands them when you want more, but it's global: depth 2
expands every marker at every level, so on a wide payload expect to trim by
hand. When a level reaches no leaf at all, stderr says `every field here returns
an object (--depth selects inside them)` — the draft is runnable and fetches
nothing, so don't hand it over as-is. A Relay connection is the usual cause and
`--depth 2` fixes it. A namespace container is the other, and `--depth` will
*not* fix that one: if the markers read `— needs arguments`, no depth reaches
them, so draft the inner field by name instead
(`gqls CardMutationRoot.activate_card -e`). A union is written as
inline fragments over its members (an interface adds one per implementor for
the fields it adds, aliased by member where two of them type the same field
differently), and deprecated fields stay in the selection
marked `# deprecated: reason` with a stderr warning naming them (tell the user
rather than pasting one silently). The `# variables` block is a fillable
skeleton: an input-object argument is expanded into its fields in schema order,
so what you hand over is directly pasteable, and the heading names the type an
expanded key no longer states (`# variables — input: CreateUserInput!`). Enums
are the one thing JSON can't express, so their values are listed under
`# enums:`.

Name a **type** and `-e` drafts the chain that fetches one, narrowed to it
where the last hop returns something broader: `gqls Cat -e` against a schema whose
only path is `Query.pets: [Animal!]!` gives you `pets { ... on Cat { … } }`. An
enum or a scalar can't be selected by any operation, and says so pointing at
`--returns` — the question that does have an answer. Don't reach for `-e` to
see what's *in* a type either; naming it plainly lists its fields.

Name an **input object** and `-e` drafts through the field that takes it —
an input is never callable but always passable, so `gqls PostFilter -e` gives
you `Query.posts(filter: $filter)`, with any other field taking one listed under
`# paths` — carrying the whole way in when the field taking it is itself
several hops out. An input *field* (`CreateUserInput.email`) drafts through its
enclosing input, and an input nothing takes drafts through the input that
holds it — you get the outer one's operation with yours expanded inside the
variables block, and a stderr line naming the carrier. Every holder is offered,
not just the drafted one, and each `# paths` entry names the input its argument
carries (`Mutation.ship(input: Shipping)`): with two holders, taking the other
path means passing a *different outer input*, so surface the choice rather than
accepting the pick. An input held deeper than the variables block expands is
refused, naming the distance — that's a real "no path", not something to work
around. The argument carrying it is supplied even where the schema
calls it optional, since a draft that omits it answers nothing. Such a draft
stays about the input: the reply gets the barest selection a server accepts and
only that input's own types are expanded, with `--depth 1` asking the payload
back. Don't reach for `-e` to see what's *in* an input object — naming it plainly
(`gqls PostFilter`) lists its fields with their types, which is the cheaper
answer.

`--via <path>` routes the draft: `gqls <schema> Issue.title -e --via Query.repository`.
Use it when `# paths` shows the draft went through a generic `node(id:)`-style
lookup — hand back any path the listing prints, or a prefix of one
(`'Query.repository > Repository.issues'`). Segments are field names,
case-insensitive; it requires `-e`.

`-e` and `-R` only act on a field the query names outright (or misspells
slightly — `Did you mean X?` on stderr says which, and is worth passing on).
A looser query — `crtusr`, `User.`, a wildcard — answers `Did you mean:`
with the matches and exits nonzero instead; re-run with the path you meant, or
show the user the list if it isn't obvious which one they want. Both respect
capitalisation the way explaining does: a record the query spells exactly, case
included, wins over one that merely ranked higher, so `gqls Card -e` drafts
against the type `Card` and `gqls card -e` against the field `Mutation.card`.

When several records are spelled exactly alike, gqls picks one, says which on
stderr (`id names 4 records; using Node.id`), drafts, and exits 0 — so the
choice is easy to miss. `-e --json` carries the runners-up as **`also_named`**
(`[]` when there was no choice). A non-empty `also_named` means the draft may be
about a different record than the user meant: surface the alternatives instead
of handing over the operation as settled.

Two things still need your judgment:

- **Ambiguous entry points.** A `# paths` block under the draft means several
  root fields return that type; the one drafted through is listed first. gqls
  picks the one with the fewest required arguments; in a federated schema
  another path may be the right one. Ask the user rather than silently
  accepting the pick.
- **Unreachable fields.** A field on a type nothing reaches is an error, not a
  guess. It fires only when the type can't be narrowed to from anything either,
  so don't invent a path — run `gqls --returns <Type>` and show what exists.

## Jump to the resolver (graphql-ruby)

Find a field, then jump to the code that implements it, via `rq`:

```sh
gqls Query.user <source> -R --code <server-dir>
# -> app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

Tries graphql-ruby conventions (resolver class, type method, mutation class)
and ranks the candidates, best convention first. A schema that namespaces its
roots is followed into the namespace — `CardMutationRoot.activate_card` finds
`Mutations::Card::ActivateCard` — and root fields try `Queries::` and
`Subscriptions::` beside `Resolvers::`. Needs the `rq` CLI installed and a
server dir that's a git repo `rq` has indexed.

A hit counts as a convention match only if it carries the name the convention
named *and* sits in the namespace it named. Both halves are checked, so a model
class that merely shares a name can no longer pose as the resolver. Everything
else is marked `(guess)`: the bare name search gqls falls back to when every
convention misses, and any hit that resembles a convention without satisfying
it. **Report a `(guess)` as a guess, never as the resolver** — `Resolvers::User`
will return a bare `class User` in `app/models`, which is a fine search result
and the wrong answer. Guesses are ordered by whether they carry the name
searched for, so the first one is the likeliest, not the confident one.

Verified matches are mutations, root fields (including a federated subgraph's
own `Query` class), fields with a custom method, and fields declared only as
`field :name, Type`, which resolve to the declaration itself.

That last case needs `rq` 0.35.2 or newer, which indexes `field` declarations
as the methods they define. On an older `rq` those fields silently find nothing.

**`no code definition found` can mean the index isn't ready.** `rq` builds it in
the background, and while it does, every candidate comes back with zero hits —
which reads exactly like a field that has no resolver. On a repo `rq` hasn't
seen before, let it finish (`rq --index <dir>`) and re-run before concluding
anything; check `rq --version` too.

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
  schema, rather than guessing. Against a URL it times `introspect: fetch`
  apart from `introspect: parse`, which settles "slow endpoint or slow gqls"
  in one run — on one live measurement, 481ms of network against 3ms of
  parsing.
- `gqls --help` prints the full help with examples.
