# gqls

**Fuzzy search over a GraphQL schema — from the terminal, for very large graphs.**

[![crates.io](https://img.shields.io/crates/v/gqls-cli.svg)](https://crates.io/crates/gqls-cli)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Point `gqls` at a schema and find the type, field or directive you're after by approximate name, or jump straight to its resolver in code. It reads an SDL file, an introspection dump, a live endpoint, or a federated supergraph, so instead of grepping SDL and guessing the exact spelling you get ranked matches — even on schemas too big to scroll, where GitHub's ~68k-line API answers in ~0.03s.

```sh
gqls user schema.graphql              # fuzzy: usr, usre, User.email all match
gqls repository https://api/graphql   # introspect a live endpoint
gqls cancel a subscription            # a phrase, matched word by word
gqls Query.user -R --code ./app       # jump to the graphql-ruby resolver
```

This README is for a person sizing the tool up or reaching for it at a prompt. [`claude/gqls-skill.md`](claude/gqls-skill.md) covers the same ground for an AI agent driving it — same facts, phrased as instructions. Read one, not both.

## Why

Nothing else combines fuzzy search with big-schema speed in a CLI. Schema viewers list and filter but don't fuzzy-match, and hosted explorers are GUIs. `gqls` fills that gap and stays Unix-composable — `-j`/`-J` emit JSON/NDJSON for every mode (a batch of piped queries takes `-J`, since a stream of `-j` arrays is nothing a parser reads).

## Install

```sh
# Homebrew
brew install dpep/tools/gqls

# Cargo (crate is `gqls-cli`; installs the `gqls` binary)
cargo install gqls-cli
```

The resolver jump (`-R`) shells out to [`rq`](https://github.com/dpep/rq); install it too if you want that.

## Usage

### Input sources
- **SDL file** — `gqls user schema.graphql`
- **Introspection JSON dump** — `gqls user schema.json`
- **Live endpoint** — `gqls user https://api.example.com/graphql` (POSTs the introspection query; add auth with `-H "Authorization: Bearer …"`, repeatable). Remote responses are cached ~1h so repeat queries don't refetch all day; a `localhost` endpoint is never cached (you're likely editing that schema). The cache key covers the request headers as well as the URL, so two runs against one endpoint under different credentials get their own schemas rather than whichever response landed first — and a revoked token stops working when it's revoked, not an hour later. Header names are lowercased and the whole key is hashed, so nothing writes a token into a path. Entries are held by count and total size, least recently used evicted first. Tune the lifetime with `GQLS_INTROSPECT_TTL` (seconds; `0` disables), `--refresh` to bypass, `--clear-cache` to wipe.
- **Auto-discovery** — omit the source and `gqls` finds a schema in the current tree (preferring `.graphqls`, then `schema.*`, then an introspection `.json`, then any SDL-looking `.graphql`; in a federated monorepo, a `supergraph*` schema wins when several exist). The tree is searched in parallel, hidden directories and known build/dependency directories (`node_modules`, `target`, `venv`, `coverage`, …) are skipped whole, and a candidate is only opened when nothing already found outranks it — so a repo with a `schema.graphql` at a sensible place is found without reading a single file. The answer is then remembered per directory for an hour so repeat runs skip the walk entirely, which is otherwise the most expensive thing a warm query does. If nothing is found beneath you, the search widens rather than failing: first to the enclosing git repository (so `gqls user` in `repo/src/components` finds the repo's schema), then to the generated directories it skipped, so a schema written by a build step is still found. Dependency directories are never searched — a schema in `node_modules` describes someone else's API. A remembered answer is dropped once the schema it names moves; `--refresh` re-walks, `GQLS_DISCOVER_TTL` (seconds; `0` disables) tunes it, and `-v` says which schema answered either way.

One caveat when reading a schema by introspection: the protocol exposes directive *definitions* but not their applications, so `directives` is always empty from an endpoint or a JSON dump, whatever the SDL applies. `@deprecated` is the exception — it travels by its own channel and is reconstructed. `-v` says so when the schema defines directives of its own.

Order is the other thing a dump doesn't carry. An abstract type's members come back in the order the source gives them — document order from SDL, the server's own from an introspection dump — so the same schema read two ways can list them differently. Membership is the guarantee; order is not.

### Federated schemas (Apollo Federation v2)
`gqls` parses subgraph SDL directly — the `extend schema @link(...)` header and `@key`/`@shareable` directives that trip up plain GraphQL parsers — so you can `cd` into a subgraph package and search its own schema. Auto-discovery follows suit: at the repo root it prefers the composed `supergraph*` schema, but run from inside a subgraph it uses that subgraph's local schema.

Parsing is as far as it goes. `@join__*` and friends come back as raw text; gqls doesn't interpret federation semantics — ownership, `@key`, `@requires`, `@external` — so read them as you would the supergraph SDL itself.

### Fuzzy search

Several words are one query, so `gqls cancel a subscription` needs no quotes, and the schema is recognised wherever it sits among the arguments. A leading kind word filters like `-k` — `gqls query user`, `gqls type User` — and gqls says on stderr when it read a word that way.

Handles abbreviations (`usr` → `User`), typos and transpositions (`usre` → `User`), and qualified `Type.field` queries. Results rank by match quality — how cleanly your query's characters sit in the name, times how much of the name they account for — and kind breaks the tie, which is what floats a root `Query`/`Mutation` field above the type it returns when both match equally well. Weak long-tail matches are cut relative to the best hit, and a query that names a record exactly cuts everything weaker outright. When the limit drops matches, the total is reported on stderr so a truncated list can't pass for the whole answer.

**Fuzzy bridges spelling, not vocabulary.** It matches names built out of your query's characters in order, so a typo or a dropped vowel still lands — but a synonym or a missing domain word is a different name, not a mangled one. `currentUser` finds nothing when the field is `me`, and `updateAddress` misses `updateUserAddress` the moment an unrelated `UPDATE_ADDRESS` outranks it. When you're guessing at a name rather than recalling one, search the word you're sure of (`address`), or write a phrase of the words the name is likely built from.

A multi-word query is matched one word at a time. Noise words (`a`, `the`, `of`, …) are dropped, and the records covering the most words win outright — `cancelSubscription` beats the many that merely echo `subscription`. When nothing covers the whole phrase, every single-word match stands.

Which means a phrase works best trimmed to its content words, the way you'd type a search-engine query, not a question: `user credit score` finds the field where `where do we expose a users credit score` buries it. Dropping noise words keeps them from scoring, but every word you leave in is a word a record can cover, and the ones you didn't mean still count.

Only whitespace opens this path: `User.email` is scored whole, and `User email` becomes the qualified form before the search runs. That rewrite has no fallback. If the first word names a type (any case) and the second names nothing on it, `gqls Post role` says `no matches for "Post.role"` — it doesn't retry as a phrase, though `gqls role` alone finds four records. Drop the type word. Quoting won't help, since the qualifier is recognised either way.

**On a schema where every name starts the same way, name the type.** A Hasura or PostGraphile schema prefixes its whole table list — `pokemon_v2_pokemon`, `pokemon_v2_item`, `pokemon_v2_move` — so a bare word matches all of it. Ranking now puts the table you meant first — a name that repeats your query, or ends with it, beats a shorter one that merely starts with it — but the rest of the list is right behind it, because on a schema like that every name really does match. Spell the type out, or add `-k query` for entry points and nothing else. Guessing at a stem and scrolling is the slow path, and it's where a beginner loses an afternoon.

A miss says what made it one. The filters in play, and what dropping them would find (`nothing returns Issue with -k query — 43 match without it`); and the schema, when gqls discovered one rather than being handed it (`no matches for "country" in examples/schema.graphql`) — "not in this schema" and "you're searching the wrong schema" otherwise read identically.

```sh
gqls createUser -k mutation      # restrict to a kind (plurals ok: mutations)
gqls User.email                  # qualified — filters to fields on User
gqls 'cancel a subscription'     # a phrase — matched word by word
```

### Name one thing and gqls explains it

Searching narrows; naming finds. When a query names exactly one of the records it matched, that record is shown on its own and annotated — its description in full, its deprecation reason, applied directives, a union's members or an interface's implementors, an enum's values with what each means, its fields with their types — an object's, an interface's or an input object's — and every path that references the type. A path counts in both directions: a field returning the type, and an argument taking it (`Mutation.createUser(input:)`) — which for an input object, never returned by anything, is the only direction it appears in at all.

```sh
$ gqls Role
gqls: 3 other matches for "Role" (--no-explain to list them)
Role  [enum]
  What a user is allowed to do.

  referenced by  @auth(requires:), User.role, CreateUserInput.role,
                 UpdateUserInput.role
  values
    ADMIN   Full access, including billing and member management.
    MEMBER  Ordinary access to the account's own content.
    GUEST   Read-only.
    OWNER   (deprecated: collapsed into ADMIN)
```

Every kind that has fields lists them, which for an object or an interface is most of what it is — reaching them through `User.` instead is a ranked search that stops at `-l`. A field taking arguments is marked `posts(…)` rather than given a column of signatures; naming the field spells them out. A type with more fields than fit is elided with the command that lists the rest (`… and 121 more — `gqls 'Repository.' -l 145` lists them all`); `--json` is never elided.

Each field carries its own applied directives, in the description's cell after any `(deprecated: …)` marker. On a federated supergraph that makes "which subgraph owns what" one command:

```sh
$ gqls Product supergraph.graphql
gqls: 2 other matches for "Product" (--no-explain to list them)
Product  [object]
  directives     @join__implements(graph: PRODUCTS, interface: "Purchasable")
                 @join__type(graph: PRODUCTS, key: "upc") @join__type(graph:
                 REVIEWS, key: "upc", extension: true)
  referenced by  Bundle.items, Query.product, Query.topProducts, Review.product
  fields
    dimensions        Dimensions!  @join__field(graph: PRODUCTS)
                                   @join__field(graph: REVIEWS, external: true)
    name              String!      @join__field(graph: PRODUCTS)
    price             Int!         @join__field(graph: PRODUCTS)
                                   @join__field(graph: REVIEWS, external: true)
    upc               String!
    weight            Int!         @join__field(graph: PRODUCTS)
                                   @join__field(graph: REVIEWS, external: true)
    crateSize         String!      @join__field(graph: REVIEWS, requires:
                                   "weight dimensions { length width unit { code
                                   } }")
    reviews           [Review!]!   @join__field(graph: REVIEWS)
    shippingEstimate  Int!         @join__field(graph: REVIEWS, requires: "price
                                   weight")
```

That's the supergraph's own text, printed, not federation semantics interpreted. A schema whose fields apply no directives renders exactly as it always did — which is every schema read by introspection, since the protocol can't report them.

On a schema small enough to read whole, `gqls User` and `gqls 'User.'` look like the same answer. The difference is what happens when the type is big: `gqls 'Repository.'` is a ranked search that hands you 20 of GitHub's 145 fields in alphabetical order, with each description clipped to whatever the columns leave; `gqls Repository` lists them in schema order with their types and docs, and names the command for the rest. Keep `User.` for searching *within* a type — `gqls 'User.*email*'`.

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

A field's default sits beside its type as the schema writes it — `direction  OrderDirection! = ASC`. It has to: a default is what makes even a non-null field optional, so the `!` alone would say the opposite.

```sh
$ gqls Commentable
Commentable  [interface]
  Anything readers can comment on.

  implemented by  Post
  fields
    comments(…)  [Comment!]!  Comments, newest first.
```

Capitalisation decides when it's the only thing separating candidates: `Role` names the enum and not `User.role`, so it explains; `role` names all three and stays a search. `--no-explain` forces the list back, and `-D` collapses an enum's values to their names and empties the description column of an input object's fields. In `--json`/`--ndjson` the record carries `match` (`"exact"` or `"corrected"`) plus `values`, `fields`, `arguments` and `referenced_by`, so a consumer gets the same facts — and each entry in `fields` carries its own `directives`.

**An exact name wins even when it's a coincidence.** Explaining is triggered by the letters, not by what you meant, so `gqls 'update address'` against a schema with a `SupportTicketDispositionLink.UPDATE_ADDRESS` enum value explains that — confidently, in full — while `UserMutation.update_user_address` sits in the matches it didn't show. The tell is the `N other matches` line above the answer: when the record you got isn't the one you were after, `--no-explain` lists what else matched.

### Many queries at once
Pipe queries on stdin, one per line, and a single run answers them all — the schema loads once instead of once per query. Against GitHub's 11,496-record schema, 20 queries drop from 0.39s to 0.18s — about twice as fast:

```sh
cat queries.txt | gqls schema.graphql -J
printf 'cancel a subscription\ndispute a transaction\n' | gqls schema.graphql -J
```

Use `-J`, not `-j`: `-j` is one complete JSON array per query, and a batch would concatenate them into something no parser reads, so a batch refuses `-j` and says to use the streaming form. Every row carries the `query` that produced it, so one stream stays untangleable, and a query that matched nothing still reports `{"query": …, "status": "no_matches"}` rather than vanishing. A single query's output is unchanged — no `query` field — so existing callers parse exactly what they always did. A piped query that names one record explains it, exactly as the same query typed as an argument would: the asymmetry was invisible, and a pipe is how an agent drives this. An explicit query beats a pipe, and `--resolve`/`--example` take one query only.

### Filter by return type
`--returns TYPE` keeps only fields whose type is `TYPE`, ignoring `[]`/`!` wrappers — the way to find a field when you know what it returns but not what it's called:

```sh
gqls --returns User                     # every field returning a User
gqls --returns User -k query            # ...just the root queries — an entry point
gqls --returns '*Payload'               # wildcards work here too
gqls post --returns Post                # combined with a name search
```

An argument's own name finds the field that takes it: `gqls term` answers `Query.search(term: String!)`, since an argument isn't a record of its own and the field is what you'd call anyway. That's a second pass, run only when nothing matched a name or a path — a Relay schema's several hundred `first` arguments never bury a search that meant a field. Name the field and you get its signature *and* what the schema says each argument is for, which is the half a signature can't carry:

```sh
$ gqls Mutation.publishPost examples/schema.graphql
Mutation.publishPost(id: ID!, at: DateTime)  -> PublishPostPayload!  [mutation]
  Publish a draft. Returns the post and any problems that blocked it.

  arguments
    id  ID!
    at  DateTime  When to publish. Omitted means now; a past time publishes
                  immediately.
```

The block appears when the schema documents at least one argument, and then lists them all — the undocumented ones are what you'd otherwise go looking for.

`gqls CreateUserInput` lists `Mutation.createUser(input:)` among what references the type, which is the other direction.

A name search can't answer this: `Query.myEmployer: Company` doesn't contain the word "Company" anywhere in its name or path. With no QUERY at all, `--returns` lists everything it matches.

When nothing returns the type outright, the filter widens to what does reach it rather than dead-ending on a precise "no": nothing returns the `Commentable` interface, yet every field returning a `Post` hands you one, so those are what you get, with a line on stderr saying so. A wildcard is left alone — it says what it means — and so is a type something already returns.

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

Combine wildcards with `-k` to narrow further (`gqls '*.email' -k input_field`).

In a qualified query, a `Type` that names a schema type (any case) becomes a hard filter — `Company.employe` searches only `Company`'s members, not every type starting with "Company". Members means fields *or* enum values, so `gqls Role.ADMIN` reaches one value and its documentation, and `gqls 'Role.*'` lists the lot. A misspelled qualifier snaps to the unique closest type (`Compnay.employe` → `Company`, announced on stderr); one that matches nothing falls back to plain fuzzy matching. That correction applies to fuzzy queries, not to wildcards — patterns match literally, so `Compnay.` finds nothing rather than guessing.

### Draft an example operation (`-e`)
Find a field, then get something you can paste into a client:

```sh
$ gqls Mutation.createUser -e examples/schema.graphql
# Create a user and return it. Fails if the email is already taken.

mutation CreateUser($input: CreateUserInput!) {
  createUser(input: $input) {
    id
    createdAt
    name
    email
    role
    avatarUrl
    metadata
    # posts: Post { … }
  }
}

# enums:
#   Role = ADMIN | MEMBER | GUEST | OWNER

# variables — input: CreateUserInput!
{
  "input": {
    "name": "<String!>",
    "email": "<String!>",
    "role": "<Role = MEMBER>"
  }
}
```

A field with optional arguments and a payload/errors convention gets two more blocks — `gqls Mutation.publishPost -e examples/schema.graphql` shows both.

`-e` and `-R` act only on a field the query actually names — the name itself, or a small misspelling of it (`createUesr`, which says `Did you mean Mutation.createUser?` above the answer). Anything looser (`crtusr`, `User.`, a wildcard) answers `Did you mean:` with the matches it found and exits nonzero: search is happy to rank the closest of what's there, but a drafted operation or a file:line both read as authoritative, so guessing which field was meant is worse than asking.

Both respect capitalisation the way explaining does: a record the query spells exactly, case included, beats one that merely ranked higher. `gqls Card -e` drafts against the type `Card`, not the field `Mutation.card`.

When several records are spelled exactly alike, the pick is said out loud rather than left to look settled — `gqls id -e` reports `id names 4 records; using Node.id` on stderr, drafts, and exits 0. `-e -j` carries the runners-up as `also_named` (`[]` when there was no choice), because a stderr line doesn't reach a JSON consumer.

The rules are deliberately conservative, because a wrong guess costs more than a visible hole:

- **Documented arguments say what they're for.** An `# arguments:` block under the operation carries the schema's own prose for each argument along the chain, since a signature says what to pass and not what passing it does. Only the documented ones — a block of names with nothing beside them says less than no block at all.
- **Arguments you must supply become variables** — nothing is inlined into the query body, and each placeholder names its type (`"<ID!>"`), so it can't be mistaken for a usable value the way `""` or `0` can.
- **Anything the server can supply is left out and listed underneath** — a nullable argument, or one with a schema default (even a non-null one, like `first: Int! = 10`). The operation runs as-is, and the knobs you skipped are still visible with their defaults.
- **One level of selection, leaf fields only.** A scalar or enum return gets no selection set at all. An object return gets its scalar/enum fields plus a `# field: Type { … }` marker per object-valued field. When that level reaches no leaf at all — a Relay connection is nothing but `edges`, `nodes` and `pageInfo` — the draft is runnable and fetches nothing, so a stderr line says `every field here returns an object (--depth selects inside them)`.
- **An `errors` block only if the schema really has one** — the payload/errors convention is common, not universal, so it's expanded only when that field exists.
- **The variables block is a fillable skeleton, not a restatement.** An input-object argument is expanded into its fields in the schema's own order, so the thing you paste into a client is the thing that shows the shape — `"<CreateUserInput!>"` only ever repeated the signature twenty lines above it. A list gets one element; a self-reference (`Filter { and: [Filter!] }`) closes as `"<Filter!>"`, since its shape is in the object directly containing it. An input the schema has no fields for stays a placeholder rather than becoming a `{}` that claims it takes nothing. Since an expanded key no longer names its type, the heading does: `# variables — input: CreateUserInput!`.
- **Enums are listed beside the block, not inside it** — JSON has no way to hold "one of these", so `# enums:` names the choice for each enum the variables reach. It's the only thing left that the skeleton can't express.
- **Abstract types become inline fragments.** A union has no fields of its own, so it's written as `... on Member { … }` over each concrete type — the only form a server accepts. An interface selects its common fields once, then adds a fragment per implementor carrying only the fields that implementor *adds*, since those are otherwise unreachable. Big unions list the first few and name the rest. Where two members give the same field name a different type (`email: String` beside `email: String!`), each is aliased by its member — `userEmail: email` — since selecting both under one response name is a shape the spec forbids.
- **`--depth N` selects more levels**, expanding the object-valued fields that depth 1 leaves as markers. It's a global knob, not a scope: depth 2 expands *every* marker at every level, so on a wide payload you get the whole tier and trim by hand. There's no way to deepen one branch.
- **Deprecated fields are flagged, not dropped.** They stay in the selection marked `# deprecated: reason`, and a stderr line names them — silently omitting a field the schema still serves is its own surprise.
- **A nested field is reached through a chain of fields from a root.** `gqls Company.employee -e` takes the shortest path from a root operation field to a `Company` and nests through it — one hop where a root returns one, and as many as it takes where a schema namespaces its roots (`Query.payroll: PayrollQueries`, with the real fields hanging off that). Shortest wins, then fewest required arguments; when several tie, a `# paths` block lists them all with the drafted one first, each written as `Query.payroll > PayrollQueries.company`. When nothing returns the type itself, something returning a broader type still reaches it through the fragment that narrows it — `Query.pets: [Animal!]!` drafts `Pet.nickname` as `pets { ... on Pet { nickname } }` — and a long chain gets a fragment at every hop that needs one. The walk is cycle-safe and capped at six hops; past the cap, and for a type nothing reaches at all, it's an error naming the distance and pointing at `--returns`, not a guess.
- **`--via` picks the route when the shortest one is wrong.** Shortest is the wrong answer wherever a root field takes an opaque ID: `Query.node(id: ID!): Node` puts every type one hop out, so `gqls Issue.title -e` drafts `node(id:) { ... on Issue { title } }` and `# paths` lists four generic lookups. The route anyone wants is three hops, and it isn't ranked low — the walk settles a type at its nearest hop and drops every later edge, so it was never a candidate. `--via` names a prefix of the route in the same notation `# paths` prints: `--via Query.repository` drafts `repository(owner:, name:) { issue(number:) { title } }`, and `--via 'Query.repository > Repository.issues'` goes through the connection instead. A segment names a field — `Type.field`, or a bare field name — matched case-insensitively; a prefix of any length costs no more than one, since at each hop the frontier is already narrowed. It works on both edges, so an input is routed the same way (`gqls AddStarInput -e --via Mutation.addStar`), and `# paths` then lists only the routes inside it. A segment that names no field says which segment and what did work; a real route that leads nowhere says so rather than claiming nothing returns the type. `--via` repairs a draft for someone who noticed the route was wrong; it does nothing for someone who pastes the first answer, and fixing *that* wants a weighted walk, not a sort key.
- **A type is drafted through the chain that fetches one.** Asking about a type is asking how to get one, so `gqls Animal -e` drafts the shortest chain reaching it, narrowed to it where the last hop returns something broader — `Query.pets: [Animal!]!` answers `gqls Cat -e` with `pets { ... on Cat { … } }`. Something no operation can select (an enum, a scalar) is an error pointing at `--returns`, which is the question that does have an answer.
- **An input object is drafted through the field that takes it.** An input is never callable, but it is always passable, so `gqls PostFilter -e` drafts the operation whose argument it is — `Query.posts(filter:)`, listed in `# paths` alongside any other field taking one. A consumer several hops from a root is reached the same way a nested field is, and its `# paths` entry carries the whole way in: `Query.admin > AdminQueries.users > UserQueries.search(where:)`. Naming an input field (`CreateUserInput.email`) drafts through its enclosing input, the thing an operation can actually name. The argument carrying it is supplied even where the schema calls it optional: a draft for `PostFilter` that quietly leaves `filter` out answers nothing. When no field takes one, the input that *holds* it is drafted instead — `AddressInput` is reached through `AddressValidationInput`, expanded where it sits inside the variables block, with a stderr line naming what carries it. Every holder is offered, not just the drafted one, and each `# paths` entry names the input its argument carries (`Mutation.ship(input: Shipping)`) — with two holders, choosing the other path means passing a different outer input, and the entry has to say which. An input held deeper than the variables block expands is refused, naming the distance, rather than drafted into an operation whose variables never mention it; an input nothing takes and nothing holds is refused outright.
- **An input draft stays about the input.** It asks where the value goes, not what comes back, so the reply gets the barest selection a server accepts (`__typename`) and only the named input's own types are expanded — `gqls PostFilter -e` was spelling out `PostOrder`, `PostOrderField` and `OrderDirection` off a sibling argument it doesn't even fill in. `--depth 1` asks for the payload back. (`--depth 0` means the same barest selection for any target; it used to be silently clamped to 1.)
- **Each section appears only when it has something in it** — no empty `# variables` block for an operation that takes none, and no `# paths` block when there's only the one the draft already shows.

Every drafted operation is parsed back with a GraphQL parser in the test suite, and a network-gated test drafts against a live endpoint and *executes* the result there — so what it prints is not just well-formed but accepted by the server it came from. `-j`/`--json` emits `{path, operation, variables, optional_args, passed_inside, arguments, enums, variable_types, deprecated, paths, also_named}` for scripting.

### Resolver jump (`-R`, graphql-ruby)
Find a field, then jump to the resolver or method that implements it, via `rq`:

```sh
$ gqls Query.user schema.graphql -R --code ./app
app/graphql/resolvers/user.rb:2  User  (via Resolvers::User)
```

Like `-e`, it only resolves a field the query names (or misspells slightly) — a looser query gets the candidate list and a nonzero exit instead, before `rq` is ever consulted.

`gqls` tries graphql-ruby naming conventions (resolver class, type method, mutation class) and ranks the candidates, best convention first; package proximity to the schema file breaks ties, so in a federated monorepo the resolver in the schema's own subgraph wins over a same-named one elsewhere.

**Its reliability varies by field kind, and it says so.** Mutations are the strong case: `Mutations::VerbNoun` is a near-universal convention, and a schema that namespaces its roots is followed into the namespace — `CardMutationRoot.activate_card` looks for `Mutations::Card::ActivateCard`. Root fields also try `Queries::`/`Subscriptions::` beside `Resolvers::`, and the bare root class (`Query#field`), since a federated subgraph names its root `Query` rather than `QueryType`. A field declared only as `field :name, Type` resolves to the declaration itself, which needs `rq` 0.35.2 or newer.

Everything else is a `(guess)`, announced on stderr — both the bare name search gqls falls back to when every convention misses, and a hit that merely *resembles* what a convention named. `Resolvers::User` will happily return a bare `class User` in `app/models`; that is a fine search result and a terrible resolver answer, so it no longer wears the convention's authority. Guesses are ordered by whether they carry the name searched for, which is what separates the one `def cards` from the nine `module Cards` above it.

### Output
Text results carry the path, the type, the kind, and the schema description — a match is usually confirmable without opening the schema:

```sh
$ gqls user examples/schema.graphql
Query.user(…)            -> User  [query]   — Look up a user by id.
User                              [object]  — An account.
ArchiveUserPayload.user  -> User  [field]
```

Three rows, not the dozen a schema this size contains the letters for: the
query spells `user` exactly, and an exact name cuts everything weaker outright.
`Query.users` and `UserError` are real matches, and neither is what you typed.

An argument list collapses to `(…)` beside other results and spells out in full
when a result stands alone — the list is for telling rows apart, and one long
signature would set the column width for every one of them.

Columns are measured in terminal columns rather than characters, so a Japanese
description wraps where it looks like it wraps; and the path and return columns
are both capped, so one 70-character generated payload type overflows its own
line instead of indenting every other row past the fold. `--json` is never
capped — a machine reader has no wall.

In a list a description is elided to one line — enough to tell one row from the next, and dropped when the columns leave no room for even that. A result shown on its own gets the whole thing, wrapped to your terminal. `-D`/`--no-description` drops descriptions, collapses an enum's values to their names, and empties the description column of an input object's fields. Every mode also supports `-j`/`--json` (pretty array) and `-J`/`--ndjson` (one record per line), which always carry the full description text. Status chatter goes to stderr, so JSON pipes clean:

```sh
gqls repository schema.json -J | jq -r '.path'
```

Exit codes, for a caller that branches on them. `0` when gqls answered — including when the answer is nothing, since "not in this schema" is a valid answer to a search and not a failure, and including an `-e` draft off a corrected spelling. `1` when a mode couldn't deliver the thing it promises: `-e`/`-R` handed a query that names no one record, a schema that won't load, a kind that isn't a kind. `2` is a usage error the argument parser rejected before gqls ran.

`-q`/`--quiet` silences the stderr status lines (results and hard errors still print); `-v`/`--verbose` adds diagnostics — cache hits/misses, and the `rq` candidates `-R` tried. Under `-R`, verbose also passes `-v` through to `rq` and streams its trace.

`--profile` prints a phase-by-phase breakdown to stderr — where a query's time actually goes, with counts alongside the timings:

```sh
$ gqls user big.graphql --profile
  cache: read       0.8ms  4.3 MB
  cache: decode     5.3ms  48501 records
  load              6.7ms  48501 records
  fuzzy scan       10.4ms  184 of 48501 records matched
  output           18.9ms
  ─────────────
  total            36.3ms
```

For a URL source, `introspect: fetch` is timed apart from `introspect: parse`,
so a slow run against a live endpoint says which half was slow — on one
measurement, 481ms of network against 3ms of parsing.

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

`script/check.sh` is the gate — formatting, clippy, and tests. Run it before pushing.

**Every timing in this README is a release build** — an installed binary, or `cargo build --release`.

Releases are cut by the shared `release` script the whole `dpep/tools` tap uses, not from this repo.

## How it works

Layered so the core is one idea — flatten every schema entity to a searchable record, and let search and output touch nothing but records:

```
src/
  model.rs        SchemaRecord + Kind (the only shared vocabulary)
  load/           SDL parse · introspection (URL/JSON) · schema discovery
  search/         the fuzzy scorer (a DP subsequence aligner + typo tier)
  example.rs      operation drafting (-e)
  style.rs        ANSI weights + the column layout for text output
  resolve.rs      field -> resolver jump (shells out to rq)
  cli.rs          clap + unified text/json/ndjson output
```

The fuzzy ranking is ported from [`rq`](https://github.com/dpep/rq)'s aligner rather than reinvented.

## License

MIT — see [LICENSE](LICENSE).
