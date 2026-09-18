# Changelog

Notable changes to `gqls`. The CLI surface — flags, output shape, exit codes —
is the public API; the crate is not intended to be used as a library.

Versions before 0.18.0 are reconstructed from release commits and tags, so the
early entries are terser than what follows.

## Unreleased

### Fixed
- **`--clear-cache` could delete files gqls doesn't own.** 0.26.0 cleared
  every file under the cache dir, and an empty or relative `XDG_CACHE_HOME` or
  `HOME` — common in CI and containers — resolved that dir against the cwd, so
  a project directory named `gqls` lost its files; a symlink inside the cache
  was followed too. The cache location now needs an absolute path, as the XDG
  spec says, and clearing deletes only the kinds of file gqls writes (and the
  `.vecs` older releases wrote), never following a symlink. If you ran
  `--clear-cache` on 0.26.0 with either variable empty or relative, check the
  directory you ran it from.
- **`--returns` given a field says so.** `gqls --returns User.name` answered
  `nothing returns User.name` — a claim about the schema, when the flag takes a
  type. It now exits 1 naming what the field returns and pointing at
  `gqls User.name -e`, which drafts a query that fetches it.

## 0.26.0 — 2026-09-18

### Removed
- **Semantic search.** gqls is fuzzy-only now: one build, no ONNX Runtime, no
  model download, no background embedding. It wasn't earning its keep: on 50
  intent queries against GitHub's schema, the default fuzzy+semantic combine
  ranked the right record *lower* than fuzzy alone (MRR 0.37 vs 0.42), and
  semantic on its own only tied fuzzy on the synonym-style queries it existed
  for. `--semantic`, `--model` and `--warm` are now usage errors (exit 2)
  that say why — drop them from scripts. `--fuzzy` still runs, since every
  query is fuzzy now, but warns that it does nothing. The `semantic` /
  `semantic-dynamic` cargo features and the `GQLS_NO_AUTOWARM`,
  `GQLS_MODEL_DIR` and `GQLS_SEMANTIC_FLOOR` variables are gone too, and JSON
  rows no longer carry `degraded`. Run `gqls --clear-cache` once to reclaim the
  embedding vectors an earlier release cached — it now empties the whole cache
  directory, files it no longer writes included. The embedding model under
  `~/.cache/huggingface` is left alone, since other tools may share it.

### Fixed
- **A word the schema already uses is no longer "corrected" into a lookalike.**
  `gqls star` explained `CheckAnnotationSpan.start` as a misspelling and hid
  `addStar` behind "76 other matches"; `gqls star -e` drafted against it.
  Across every word in GitHub's schema, 83 were corrected this way — `host` to
  `RateLimit.cost`, `the` to the country code `TH` — and a few more, like
  `bot`, stopped explaining the record they named exactly because a bogus
  correction tied with it. A correction now applies only where the part it
  corrects isn't a word of some name; `strat` and `Usr.email` still correct.
- **A miss that a filter caused counts its matches in plain English.** It said
  `43 match without it`, and `1 matches` for one.
- **`--profile` JSON rounds milliseconds to two decimals** rather than printing
  float noise like `0.10579200000000001`.
- **`-e` warns only about deprecated fields the draft selects.** A field left as
  a commented `# name: Type { … }` hole, or dropped from an implementor's
  fragment because the interface already selected it, was named in the
  `deprecated:` line with nothing in the draft to find. On the GitHub schema at
  `--depth 2`, 125 of 300 root drafts (42%) named at least one such field; none
  do now. Fields that really are selected keep both their inline `# deprecated:`
  flag and their place in the line.
- **The `deprecated:` line no longer says `(flagged inline)`.** The drafted field
  itself is deprecated often enough to matter and never carries an inline note —
  a note goes on a selection, and the target is the operation. It reads
  `(drafted anyway)` now, which says the thing worth saying: nothing was
  silently dropped.

## 0.25.0 — 2026-09-14

### Added
- **`--via` picks the route `-e` draws.** The walk settles each type at its
  nearest hop, so on a schema with a global-ID lookup every type is one hop from
  `Query.node` and `gqls <gh> Issue.title -e` drafts
  `node(id:) { … on Issue { title } }` — the route through a repository wasn't
  ranked low, it was never a candidate. `--via` names a prefix of the route in
  the notation `# paths` prints, and the walk takes only the field named at each
  of those hops: `-e --via Query.repository` gives
  `repository(owner:, name:) { issue(number:) { title } }`, and
  `--via 'Query.repository > Repository.issues'` goes through the connection.
  Hand back any path the listing prints, at any length, including an input's
  `Mutation.addStar(input:)`. It repairs a draft for someone who noticed the
  route was silly — `# paths` listing four generic ID lookups is the only tell —
  and does nothing for someone who pastes the first answer. Fixing that default
  is a weighted walk rather than a sort key, and is not this.

### Fixed
- **A deprecated field is named once in the `-e` warning**, not once per place
  the draft selects it. A type selected under two fields was listed twice; the
  inline flags were right, the summary line wasn't.

## 0.24.0 — 2026-09-13

### Changed
- **`-e` says when a draft is too large to paste.** A selection set fans out by
  the branching factor of the schema, so `--depth` is cheap to type and
  expensive to render — GitHub's `Repository` is 6KB at depth 1 and 200KB at
  depth 3. The size is invisible until it has scrolled past, so a draft over
  32KB now says how big it is, and names `--depth` when that's what got you
  there. Not a cap: a large schema can legitimately want a large draft. The
  threshold is measured — across four schemas every draft anyone would paste
  came in under 9KB, and the smallest unusable one was 88KB.
- **A fuzzy score is now the fraction of a perfect match**, where perfect means
  the query *is* the name — one scale, meaning the same thing whatever the
  query's length, the name's length, or which branch produced it. A clean word
  inside a longer name used to score an order of magnitude below a prefix match
  of the same quality, so the weak-tail cut dropped it outright: `gqls <schema>
  disput` never reached `Mutation.in_app_disputes` at any limit. It ranks 85th of
  116 now — still past a default page, but findable.

  **`--json`/`--ndjson` consumers:** scores are smaller and differently shaped.
  A perfect match is 1000, plus up to 300 where a `Type.` qualifier names the
  right parent; the old top was 1060. Note that only the fuzzy ranker writes on
  this scale — `--semantic` reports a cosine in 0..1 and the default combine of
  the two reports a rank-fusion score around 0.03, as both always did. Sort on
  `score` within one query; don't compare it across two.
- **Kind — root field, type, leaf field — no longer adds to the score**; it
  breaks ties between matches of equal quality instead.
- **A name that repeats the query, or ends with it, outranks a shorter name that
  merely starts with it.** On a schema where every name shares a prefix —
  anything Hasura-generated — `gqls <schema> pokemon` returned four unrelated
  tables ahead of `pokemon_v2_pokemon`, which sat at rank 21 with the object type
  at 899. Measured over 2309 queries on four schemas, 292 changed top hits are
  better and 17 worse by an independent referee; the table-lookup case went from
  17 unfindable to 7.
- **`--profile` times the network apart from the parse** when the source is a
  URL. Both were inside one `load` span, so a slow run against a live endpoint
  gave no way to tell a slow endpoint from slow gqls — on one measurement the
  split was 481ms of network against 3ms of parsing.
- **The one-time embedding pass no longer guesses "may take a minute".** It
  reports an estimate measured from the run's own rate, and now prints to
  non-terminal stderr every 15s — a piped or CI run could not tell slow from
  hung, and on the largest schema tested this is minutes, not one.
- **`--json`/`--ndjson` carry `degraded: true`** when semantic ranking fell back
  to the hash embedder; absent otherwise. The warning was stderr-only, which
  never reaches a caller parsing stdout.
- **Cached introspection responses are bounded by count and size**, least
  recently used evicted, rather than by age alone.
- **`-e`/`-R` say out loud when several records are spelled exactly like the
  query.** `id` names `Character.id`, `Location.id` and `Episode.id`; the pick
  used to look settled. It still drafts and still exits 0 — the runners-up go to
  stderr, and `-e -j` gains `also_named` (`[]` when there was no choice), because
  a stderr line doesn't reach a JSON consumer.
- **`-e` says when one level of selection reaches no leaf at all.** A Relay
  connection has nothing but object-valued fields, so the default draft runs and
  fetches nothing; the note points at `--depth`. The draft itself is unchanged.
  Said only where `--depth` can actually help: a namespace container whose
  fields every one takes required arguments draws the same empty-looking
  selection, but no depth selects through them — draft the inner field by name.
- **`--depth` is capped at 6.** A selection set fans out geometrically, so a
  mistyped depth was a stack overflow or a gigabyte of stdout rather than a slow
  answer: GitHub's `Repository` drafts 1.8MB at depth 4 and 877MB at 7, chime's
  `User` 90MB at 12. Past the cap nothing is a document you could paste. A
  `--depth` above it now drafts at 6 and says so on stderr; scripts passing a
  larger number get a smaller draft instead of a crash.

### Added
- **`--resolve` follows a namespaced root.** A field on `CardMutationRoot` looks
  for `Mutations::Card::ActivateCard`, and `Queries::`/`Subscriptions::` are
  tried beside `Resolvers::` for root fields. Across six field shapes on a real
  server repo the tally went from 1 clean hit, 1 near miss, 3 misses and 1
  confidently wrong answer, to 5 clean hits, 1 honest miss and none wrong.
- **`-v` says when introspection can't report applied directives.** The protocol
  exposes directive *definitions* but not their applications, so `directives` is
  always empty from an endpoint or a dump however many `@auth`s the SDL applies —
  a limit of the protocol, not of the schema. Said only when the schema defines
  directives of its own, so it stays quiet on the five every server defines.
  `@deprecated` is the exception and is reconstructed.

### Fixed
- **`-e` could overflow the stack on a legal schema, at the default depth.** The
  payload/errors convention expands an `errors` field at the last level too, and
  did it unconditionally — so a schema where the errors chain returns to a type
  already being selected (`Payload.errors -> UserError.errors -> Payload`, or
  just `Payload { errors: Payload }`) recursed until the process aborted with
  exit 134. The convention still expands; it just no longer re-opens a type
  open above it, which is a cycle with no level left to spend.
- **`gqls` announced "no matches" over rows it had just printed.** The miss
  message was decided by the fuzzy match count, but the default ranking also
  prints rows that matched by meaning alone — so stderr said the schema had
  nothing while stdout listed fields, and an agent reading one reached the
  opposite conclusion of an agent reading the other. Rows now get a message
  that says what they are.
- **A single `-J` query that matched nothing printed nothing at all.** Zero rows
  on a row-per-line stream is zero bytes, so the caller could not tell a miss
  from a query that never ran, and the `degraded` flag marking weak ranking had
  nowhere to ride. It now emits `{"status":"no_matches"}` — with `degraded` when
  it applies — the way a batch always has. `-j` is unchanged: an empty array was
  already a complete answer.
- **An argument answers when nothing matched a *name*, not when nothing matched
  at all.** The first pass matches names *and* qualified paths, so a weak
  subsequence of someone else's path counted as an answer and suppressed the
  argument pass entirely. `gqls <github> until` returned `CheckRun.title` at
  0.128 of a perfect match and hid the seven fields that take `until:` at 1.0 —
  one unrelated type in the schema was enough to do it. Path matches still stand
  when nothing takes the query as an argument either, and an argument still
  never outranks a name: `first` returns the fields named for it, not the 348
  that take one.
- **One long field name no longer indents every other row of a fields block.**
  The search table's columns were capped last round and the block you get by
  *naming* a type wasn't, so a single generated name or return type set the
  width for every row under it — a 35-column name and a 38-column type gave
  every line of one real type an 81-column indent against an 80-column
  terminal. Both columns are now capped the way the table's are (24 and 28,
  the ninetieth percentile on two production schemas), and a cell wider than
  its column overflows its own line instead of moving everyone else's. Across
  those schemas the share of field rows running past 80 columns falls from 14%
  and 24% to 5%. A schema with no outlying names renders unchanged.
- **The introspection cache ignored the credentials that fetched a schema.** It
  keyed on the URL alone, so the first response to succeed was replayed for every
  later run against that URL whatever headers were supplied — or not supplied.
  A revoked or rotated token kept working for up to an hour, a CI check existing
  to assert *auth is enforced* passed falsely, and two callers sharing a URL with
  different credentials could be served each other's schema. The key covers the
  request headers now — sorted, names lowercased, hashed, never written to the
  path. Entries cached under the old rule are unreachable and refetch once;
  nothing for you to do.
- **A parsed-record cache file that won't decode says so under `-v`** before
  falling back to reparsing, the way the introspection cache already did.
- **`-e` leaked a multi-line deprecation reason out of its comment.** A `#`
  comment ends at the newline, so only the reason's first line stayed commented
  and the rest landed in the selection set, where a server reads it as field
  names. On one real schema that broke **80 of 2083 drafts** — all from a single
  field, because it's selected under every actor chain. Every free-form string a
  draft quotes is flattened to one line now, including an optional argument's
  block-string default, which was independently broken.
- **A type's fields block showed no directives.** The type carried its own and a
  named field carried its own, but the list between them carried none — so
  working out which subgraph owns each field of a federated type meant naming
  every field, one command each. Directives sit in the description's cell now,
  after any `(deprecated: …)` marker, because the two are disjoint in practice:
  across 14,806 field records in four schemas, exactly one carries both. A schema
  whose fields have no applied directives renders exactly as before — which is
  every schema loaded by introspection, since the protocol can't report them.
- **A query that names a record exactly no longer reports weaker matches
  alongside it.** `gqls Query.me` claimed two other matches on a fully qualified,
  unique path: the weak-tail cut compared the *ranking* score, which carries the
  boost every member of a named type is handed alike, so a more precise query cut
  its tail *less*. The cut compares match quality now, and an exact name cuts
  everything weaker outright rather than by ratio — a ratio can't express it,
  since a long prefix scores below the subsequence ceiling and the tiers overlap
  as numbers. Unqualified queries narrow the same way: `gqls me` returns the
  records actually named `me` instead of 184 fuzzy matches. Phrase queries and
  argument-name matches are unaffected.
- **`--resolve` built an unusable class name from a snake_case field.**
  `create_transfer` became `Mutations::Create_transfer` rather than
  `Mutations::CreateTransfer`, so the convention the README calls the strong case
  silently failed on every snake_case mutation — 36 of 56 on one production
  schema — and fell through to the field *declaration*, one hop short of the
  code. One helper feeds every convention, so all of them were affected.
- **`--resolve` presented a resembling hit as a verified match.** It checked the
  namespace half of a candidate and not the name half, so an unqualified
  candidate like `MeResolver` accepted every `*Resolver` in the repo — ten
  unmarked results for `Query.me`, none of which implemented it. A symbol that
  isn't named what the convention named is a `(guess)` now.
- **`--resolve` guesses are ordered by whether they carry the name searched
  for**, rather than in an order that only looked like confidence. On one case
  the correct file moved from guess #9 to #1.
- **`-e` on a multiply-held input offered only one holder.** An input nothing
  takes is drafted through one that *holds* it, and `gqls CheckImage -e` reported
  only `UploadFrontImageInput` while `UploadRearImageInput` was an equally valid
  and semantically different answer. Every holder appears under `# paths` now,
  each naming the input its argument carries (`Mutation.ship(input: Shipping)`),
  so choosing another path visibly means passing a different outer input. This is
  common: 48 of the 92 such inputs across two production schemas have more than
  one holder. The operation drafted may differ from before — it's now picked by
  the same chain ranking as every other `-e` path.
- **`-e` refuses an input held deeper than the variables block expands**, naming
  the distance, rather than drafting an operation whose variables never mention
  the input you asked about.
- **`-e` and `--resolve` ignored capitalisation where search respects it.**
  `gqls Card` explains the type; `gqls Card -e` drafted against the *field*
  `Mutation.card` — a container whose fields all need arguments, so the body was
  nothing but comment markers. Ranking is case-blind, and only the explain tier
  applied the rule that GraphQL capitalises types and not fields. A hit the query
  spells exactly now wins in all three modes; where several records share a name
  ranking still picks, and a lowercase or misspelt query is unaffected.
- **A miss blamed the query for what the filters did.** `--returns Issue -k query`
  said `nothing returns Issue` while 43 fields returned one, so the type read as
  unreachable. A miss now names the flags in play and what dropping them would
  find — `nothing returns Issue with -k query — 43 match without it` — and names
  the schema when gqls discovered one, since "not in this schema" and "you're
  searching the wrong schema" are otherwise the same sentence.
- **`score` reported `0.0` for a record ranking never scored.** Naming a record
  explains it even when `-l` sorted it off the ranked page, and the row then
  carried a zero — a legal score, so nothing distinguished "ranked lowest" from
  "never computed", and the same query at two limits reported a real score and
  then a zero. It is `null` now; the key is always present, so the document
  shape is unchanged.
- **`-j` emitted unparseable JSON for piped queries.** `-j` is one complete array
  per query and a batch answers many, so the output was concatenated top-level
  values no parser reads. A batch refuses `-j` and points at `-J`, the streaming
  form.
- **An SDL description is no longer rewritten as if it were code.** The three
  normalisations gqls applies before parsing — the federation `extend schema`
  header, a description above `schema { … }`, and a pre-2018 `implements A, B`
  list — searched raw text for their keywords, so documentation that spelled one
  got edited. A description *opening* with the word `schema` was the worst: it
  deleted back to the previous string literal, dropping the type defined in
  between out of the schema entirely, or failing the parse outright.
  Descriptions and `#` comments are left alone now.
- **A schema file is read for what it holds, not what it's called.** An
  introspection dump saved as `schema.graphql` — what `curl … > schema.graphql`
  writes — came back as `parsing SDL: Parse error at 1:1`, blaming a syntax error
  in a file with nothing wrong with it. The first non-whitespace byte picks the
  loader now, and SDL under a `.json` name reads as SDL for the same reason. A
  JSON file that is no dump still says so, naming the file. A dump with *no*
  extension still can't be passed on the command line: an argument is only
  recognised as a source by its extension or its scheme.
- **`@deprecated` with no reason now reads "No longer supported"** — the spec's
  default for the argument, and what a conforming server reports for the same
  schema — where gqls said "deprecated", rendering as the stutter
  `deprecated  deprecated`. The same schema gave two different answers depending
  on whether it was read as SDL or by introspection.
- **An empty description is no longer presented as documentation.** A schema
  writing `""` reached `--json` as `"description": ""`, and on an argument it
  promoted an entire `arguments` block claiming the schema said what the argument
  was for. Both loaders drop it now, as introspection already did for a record's
  own description.
- **A saved introspection *failure* reports what it recorded.**
  `{"data": null, "errors": […]}` — what `curl … > schema.json` writes with an
  expired token — said "is not a GraphQL introspection dump". A dump is a saved
  response, so the file and URL loaders read it through one path now.
- **An interface list written the pre-2018 way** (`type X implements A, B {`),
  still emitted by graphql-ruby, now loads. graphql-parser rejected the whole
  file and blamed "end of input" at the second interface name.
- **Discovery reports a tie between two schemas in the same directory** under
  `-v`. It counted only candidates in *other* directories, so the closest
  ambiguity was the silent one.
- **The return-type column is capped**, the way the path column has always been
  capped at 48. One generated 70-character payload type set the column for a
  whole page and pushed every `[kind]` tag past 130 columns; it now overflows its
  own line and leaves its neighbours alone. Both caps are calibrated at about the
  90th percentile of real schemas.
- **Column widths are measured in terminal columns, not Unicode scalars**, so a
  description written in Japanese wraps where it looks like it wraps — lines that
  claimed to fit 80 columns were drawing 93. CJK prose written *without* spaces
  still doesn't wrap at all, since wrapping breaks on whitespace; that needs line
  segmentation and hasn't been done.
- **An elided `arguments` block no longer prints a pointer back at itself.** It
  said `gqls '<field>.' -l N` "lists them all", and running that re-printed the
  same elided block. It now just says how many are left.
- **An argument name buried the records actually named that.** 0.23.3 scored an
  exact argument match a flat 200, which beats the typo tier (190 at distance
  one) and much of the weak subsequence tier — so `gqls input` returned the
  mutations *taking* an `input:` argument instead of the input objects *named*
  `…Input`, and raised the weak-tail floor enough to cut the name matches
  entirely. `names`, `period`, `targets` and a dozen others went the same way on
  a Relay-style schema. Arguments are a second *pass* now rather than a lower
  tier: names and paths are matched first, and argument names only if that found
  nothing — which is what the feature always claimed, and what no single
  constant could deliver, since "below every name match" is a property of the
  result set and a score sees one record. The same change fixes a phrase query
  losing its name matches outright (`refund order` answered only
  `Mutation.refundPayment`, because `orderId` covered `order` and the phrase
  filter keeps just the records matching the most words). Finding a field by an
  argument it takes is unaffected: every query the feature rescued still answers.

## 0.23.3 — 2026-09-10

### Fixed
- **Naming `Query` or `Mutation` listed no fields.** A root type's fields carry
  their operation's kind rather than `Field`, so the fields block skipped them —
  leaving the one type whose fields are most worth listing showing none at all.

### Added
- **An argument's name finds the field that takes it.** `gqls followRenames`
  found nothing: arguments aren't records, so their names were unsearchable and
  the README promised otherwise. The field that takes one is now matched by it —
  which is what you'd call anyway, and naming that field says what the argument
  is for. It ranks below every name and path match and so only surfaces when
  nothing else matched, which is what keeps a Relay schema's 348 `first`
  arguments from burying a search that meant a field.
- **`-e` reaches an input object that only another input holds.** An input is
  drafted through the field that takes it, and one nothing takes was refused —
  though it may still be *held* by one that something takes
  (`AddressValidationInput { address: AddressInput! }`), which is exactly where
  a caller needs to see it go. The draft is the outer input's, with the one you
  asked about expanded where it sits inside the variables block, and a stderr
  line saying which input carries it. That takes one production schema's
  draftable input objects from 198 of 230 to all 230. `--json` carries
  `passed_inside`.

## 0.23.2 — 2026-09-09

### Added
- **`-e` reaches a target through as many hops as it takes.** It only ever
  drafted a field or type one hop from a root operation field, which on a schema
  that namespaces its roots (`Query.early_pay: EarlyPayQueryRoot`, with the real
  fields hanging off that) is a small fraction of what's there. Across the two
  production schemas gqls is swept against, 340 of 1737 types and 643 of 1102
  drafted before; 1734 and 1101 do now, and everything still refused is
  genuinely unreachable rather than too deep. It walks the type graph out from
  the roots and nests through the shortest chain it finds, on both edges — a
  field or type by what returns it, an input object by what takes it, which
  takes one schema's draftable input objects from 168 of 230 to 198. The walk is
  cycle-safe, prefers the chain asking fewest arguments among equals, and stops
  at six hops — a cap neither schema reaches, since every type either one can
  reach at all lands inside it. Past the cap the refusal names the distance, so
  "too deep" reads differently from "not there".

  `via` and each `# paths` entry now spell the whole chain, `Query.early_pay >
  EarlyPayQueryRoot.status`, in text and `--json` alike. A one-hop path is
  unchanged; the one label that moves is a nested input consumer's, which used
  to name only the field taking the input and now carries the way in as well.

  Still refused: an input object that no field takes and only another input
  object holds — a third edge, and 32 of that schema's 230.
- **Arguments carry what they're for.** Both loaders dropped argument
  descriptions — the introspection query had always asked for them — so the one
  thing a signature can't tell you was the one thing gqls couldn't show:
  `owner: String!` never says it wants a login. Naming a field now lists its
  arguments with the schema's prose for each, `-e` prints an `# arguments:`
  block for the ones along the chain, and `--json` carries them on both. Only
  documented arguments appear; a block of bare names would restate the
  signature. Arguments are still not searchable by name — they aren't records —
  and the record cache re-reads once on upgrade, as it does whenever its format
  gains a field.
- **A piped query explains what it names.** The explanation tier — the
  annotations, and the `match`/`values`/`fields`/`referenced_by` keys — was
  switched off for stdin, so the same query answered differently depending on
  whether it arrived as an argument or down a pipe, with nothing saying so. A
  pipe is how an agent drives gqls, which is where the fuller answer is worth
  most. `--no-explain` still forces the list.
- **A `Type.` query with nothing to enumerate answers with the type.** A union
  has no fields, so `gqls 'SearchHit.'` reported no matches while the union it
  named sat one line away with its members and what references it. An
  enumeration that finds something still never explains — a trailing dot is one
  character from the type's own name, and `Query.` means the root fields.
- **Naming an object or an interface lists its fields.** Only an input object
  did, on the reasoning that an object's fields show up in a selection set
  somewhere else — they don't, and reaching them meant a second search
  (`User.`) that ranks and truncates rather than listing. Each field comes with
  its type, description and deprecation; one taking arguments is marked
  `posts(…)`. Past a couple of dozen the list is elided with the command that
  spells out the rest (`… and 121 more — `gqls 'Repository.' -l 145` lists them
  all`); `--json` carries every field and every argument signature.
- **`-e` drafts an operation for a type.** `gqls Animal -e` used to answer
  "can't draft an operation for a union". Asking about a type is asking how to
  fetch one, so it now drafts the root that reaches it — narrowed to it where
  that root returns something broader, so `gqls Cat -e` is
  `pets { ... on Cat { … } }`. An enum or a scalar still can't be selected, but
  says so pointing at `--returns`, which is the question with an answer.
- **`--returns` widens rather than dead-ending.** Nothing returns the
  `Commentable` interface, so the filter answered nothing — while every field
  returning a `Post` hands you one, and `-e` drafts exactly that. When no field
  returns the named type outright, the filter now matches what narrows to it,
  and says so on stderr. A wildcard is left alone, and so is a type something
  already returns.

### Fixed
- **A deprecated object-valued field drafted an operation that doesn't parse.**
  The `# deprecated` note was appended to the field name, so on a field with a
  selection set the `{` landed inside the comment and the document came out
  unbalanced. It fired at the default depth, since the errors convention is
  always expanded: 8 of 130 root operations in one production schema were
  affected.
- **`-e` drafted operations a conformant server rejects.** Two members of an
  abstract type giving the same field name a different type — GitHub's
  `Organization.email: String` beside `User.email: String!` — were selected
  under one response name, which the spec forbids (§5.3.2); graphql-js rejects
  it outright and graphql-ruby warns that it will. Each is aliased by its
  member now (`userEmail: email`). Every root of two production schemas, at
  three depths, validates strictly.
- **`-e` orphaned a selection set it meant to drop.** A field an interface had
  already selected was dropped from its implementors' fragments one line at a
  time, so a field with a selection set left its body and closing brace behind
  and the operation didn't parse.
- **`-e` dropped an interface's implementors.** One whose added fields are all
  object-valued has nothing but `# field: Type { … }` markers, and the
  interface path dropped every marker — so the fragment came out empty and the
  implementor vanished, with nothing saying it existed. A union in the same
  position kept its markers, so the two abstract paths disagreed.
- **`-l` decided whether an answer was a list or an explanation.** Whether a
  query names exactly one record was read off the top `-l` rows, so the display
  limit changed the answer and, on a big schema, which record got explained —
  `user -l 1`, `-l 2` and `-l 3` each answered differently, one of them
  explaining a deprecated field as an exact match out of 84. The decision now
  runs over every record the filters admit, and the count of what it set aside
  is out of the whole match set.
- **`--profile` printed nothing with `-e` or `-R`.** Both return before the
  report block.
- **The semantic index was promised when nothing was building it.** With
  `GQLS_NO_AUTOWARM` set — or when no warm could be spawned — the "building the
  semantic index in the background" line printed every run regardless, forever.
- **An annotation row stayed a row.** `implemented by` was uncapped while
  `referenced by` elided past six, so GitHub's `Node` printed 128 implementor
  names. `-D` also swapped `values` and `referenced by`, though it only chooses
  between an enum's block and collapsed forms.
- **`@join__*` matched nothing.** A pattern only addressed paths when it held a
  `.`, so pasting back a directive the way gqls prints it silently missed.
- **`--returns '[Card!]!'` matched nothing.** Wrappers came off the schema side
  of the comparison but not off the flag, so the type as the schema writes it
  was the one spelling that failed.
- **`-l 0` reported "no matches"** for a query with five of them: a miss is
  nothing matching, not an empty page.
- **The README and `--help` claimed arguments were searchable.** They aren't
  records, so `gqls followRenames` finds nothing; you reach an argument through
  the field that takes it. The claim is gone rather than the gap papered over.
- **`--returns` with no QUERY reported `no matches for "*"`.** The wildcard is
  gqls's own — nothing the caller typed — so the miss now names the filter:
  `nothing returns Zork`.
- **`-e` reaches a field through the fragment that narrows to it.** A field was
  only drafted when some root field returned its enclosing type outright, so
  `Pet.nickname` reported "isn't reachable in one hop" whenever the only path
  ran through a union or another abstract type — a query people write daily,
  and one `gqls Query.pets -e` already drafted. A root whose runtime types
  overlap the field's parent now counts, and the draft says how:
  `pets { __typename ... on Pet { nickname } }`. A root returning a type that
  *implements* the interface still selects the field directly, with no
  redundant fragment.
- **A truncated result list says so without `-v`.** The `N matches; showing top
  M` count was a verbose-only diagnostic, so a list cut off at `-l` (20 by
  default) read as the complete answer — naming a type with more fields than
  that quietly lost the rest. It's normal stderr status now, and it no longer
  fires when explain mode collapsed the list, which reports its own hidden
  matches.
- **A piped batch now answers each query as it arrives.** `read_queries` drained
  stdin to EOF before searching anything, so `producer | gqls schema.graphql -J`
  stayed silent until the producer closed — and never printed at all for a
  producer that doesn't. The output bytes are identical either way, which is why
  this went unnoticed; the new test asserts the timing instead.

## 0.23.1 — 2026-08-20

### Fixed
- **Input field defaults were dropped at load time.** `role: Role = MEMBER` loaded as plain
  `Role`, which left a defaulted field indistinguishable from a mandatory one —
  `direction: OrderDirection! = ASC` may be omitted, and the `!` on its own says
  the opposite. `SchemaRecord` carries a `default` now, both loaders populate
  it, and it shows beside the type: `direction  OrderDirection! = ASC` when you
  name the input, and `"<OrderDirection! = ASC>"` in an `-e` skeleton.
  `--json` carries it on input-field records and inside `fields`.

  On upgrade the parsed-record cache re-reads once, since its on-disk format
  gained a field. Nothing to do — a miss costs milliseconds.

## 0.23.0 — 2026-08-20

### Added
- **Naming an input object lists its fields.** It was the one composite kind
  whose members the explanation never showed — an enum gave its values, a union
  its members, an input object a single line — so `gqls CreateUserInput` said
  nothing about what goes in one. Each field is listed with its type, its
  description and its deprecation, and `--json` carries the same under a
  `fields` key.
- **`-e` drafts an input object through the field that takes it.** An input is
  never callable but always passable, so `gqls PostFilter -e` now drafts
  `Query.posts(filter: $filter)` instead of refusing. Other fields taking one
  are listed under `# paths`; an input field (`CreateUserInput.email`) drafts
  through its enclosing input. The argument carrying it is supplied even where
  the schema calls it optional — a draft for `PostFilter` that leaves `filter`
  out answers nothing. Such a draft stays about the input: the reply gets the
  barest selection a server accepts, and only the named input's own types are
  expanded, so `PostFilter` no longer spells out the three types hanging off a
  sibling argument it doesn't fill in. `--depth 1` asks for the payload back.

### Changed
- **The `-e` variables block is a fillable skeleton, and `# input types:` is
  gone.** `"<CreateUserInput!>"` never said anything the `($input:
  CreateUserInput!)` signature above it didn't, which is exactly why a second
  block had to spell the type out in SDL — so the shape got printed twice, in
  two notations, neither of which you could paste into a variables pane. An
  input-object argument is now expanded into its fields, in the schema's own
  order, inside the block you paste. A list gets one element; a self-reference
  (`Filter { and: [Filter!] }`) closes as a placeholder, since its shape is in
  the object containing it; an input the schema has no fields for stays a
  placeholder rather than becoming a `{}` that claims it takes nothing.

  What's left over is what JSON can't express, and it's named for what it is:
  `# enums:` lists the values for each enum the variables reach. And because an
  expanded key stops naming its type, the heading does — `# variables — input:
  CreateUserInput!`.

  **`--json` consumers:** `input_types` is replaced by `enums` (a list of
  `"Role = ADMIN | MEMBER"` strings) and `variable_types` (a list of
  `"input: CreateUserInput!"`). `variables` now nests instead of holding a flat
  type placeholder per key.
- **`--depth 0` now means the barest valid selection** rather than being
  silently clamped to 1. It's what an input-object draft takes by default, and
  what `--depth 0` always looked like it should do.
- **`referenced by` counts arguments, not just return types.** It scanned what
  *returns* a type and nothing else, so it reported nothing at all for input
  objects, which are only ever passed in. An argument reference reads
  `Mutation.createUser(input:)`, naming where the value goes. Applies to every
  kind: `Role` now shows `@auth(requires:)` too.
- **The library API is a fifth of its former size — 148 public items down to
  30.** `lib.rs` re-exported all twelve modules, so most of that surface was
  public by default rather than by decision — which the header above already
  said was never the intent. `logging`, `paths`, `profile`, `render`,
  `resolve`, `semantic` and `style` are now crate-private, along with 25 items
  inside the five modules that callers do use. `model::SchemaRecord`,
  `model::Kind`, `load::sdl`, `load::LoadOptions`, `search::search`,
  `search::Hit`, `search::Filters`, `example::build` and `cli::run` stay public.

Nothing to do on upgrade: the `gqls` binary and its CLI surface are unchanged.

### Removed
- `semantic::search`, a one-shot convenience wrapper, has never been called —
  the CLI builds its own `Session`. It was dead from the initial commit and
  invisible to `dead_code` only because `semantic` was a public module.

### Fixed
- **A drafted selection set is never all comments.** A type whose fields are
  every one object-valued got a selection set holding only `# field: Type { … }`
  markers, which no server will parse. It now selects `__typename` alongside
  them, the same answer already used for a type the schema doesn't detail.
- `paths::temp_dir` is gated on `_semantic`, matching its only caller. It was
  compiled, and dead, in the fuzzy-only build.

### Internal
- `unreachable_pub` is on. A `pub` item inside a private module is reachable
  from nowhere, and `pub` is precisely what makes `dead_code` skip an item — so
  the two together were hiding the two entries above.

## 0.22.0 — 2026-08-13

### Changed
- **Embedding vectors are 256-dimensional, up from 64 — every schema re-embeds
  once on first use after upgrading.** A 4602-record schema takes about a
  minute and its vector cache grows from 1.2 MB to 4.8 MB. `--profile` now
  reports the width and footprint (`4602 vectors x 256d, 4.5 MB`) so the trade
  is visible. The width buys the calibration the new relevance floor needs;
  ranking was already fine at 64.

### Added
- Colour in text output, in three weights and no hue: **bold is the identity**
  (the matched path), **plain is the answer** (a field's return type, and the
  description of a record you named), **dim is the apparatus** (arguments, the
  arrow, the kind tag, a description you're scanning past). Red marks a
  deprecation and nothing else. All three weights are relative to your own
  foreground, so none of them can clash with a terminal theme the way a colour
  would. Suppressed when stdout isn't a TTY or `NO_COLOR` is set, and it only
  ever adds escapes, so the visible characters are identical either way.
- Several positional words are one query, so `gqls cancel a subscription` needs
  no quotes, and `gqls query User` no longer fails by reading `User` as the
  schema. A leading kind filters — `gqls query user`, `gqls type User` — the
  same as `-k`, whether the shell split the words or you quoted them. gqls says
  on stderr when it read a word that way; setting `-k` keeps the word in the
  query instead. The schema is recognised wherever it sits in the arguments.
- An explanation of an enum lists its values with their descriptions, so
  reading `Role` answers what `ADMIN` grants without a second search. `-D`
  collapses it to the names, and an enum whose values are undocumented collapses
  on its own.
- `--json`/`--ndjson` carry the same facts as the text explanation: `values` and
  `referenced_by` join the record when it's the one a query named. The others
  (`deprecated`, `directives`, `possible_types`) were always serialized.
- A query that names exactly one of its matches now gets that record on its own,
  annotated: the deprecation *reason* rather than a bare marker, applied
  directives with their arguments, a union's members or an interface's
  implementors, an enum's values, and what references a type. Searching narrows;
  naming finds. `--no-explain` forces the list back, and a stderr note says how
  many other matches there were.
- Capitalisation decides when it's the only thing separating candidates. `Role`
  names the enum and not `User.role`, so it explains; `role` names all three and
  stays a search. `-e`/`-R` are unchanged and stay case-insensitive, because
  `createuser -e` should draft rather than lecture.
- `match: "exact" | "corrected"` on the named record in `--json`/`--ndjson` —
  the discriminator for "this is an explanation, not a list", absent otherwise.
  Additive: the array shape and every existing field are unchanged.
- `-e` leads with what the field does, as a comment above the operation. It has
  already committed to one field — it refuses to draft unless the query named
  it — so the whole description goes in, wrapped and uncapped.

### Changed
- A query the schema can't answer returns nothing. Semantic ranking scored
  every record and the tail cutoff was relative, so nonsense still got its best
  noise back — `gqls zzzqqq` returned a record at cosine 0.011. There's an
  absolute floor now, and reaching it meant widening the vectors: at 64
  dimensions answerable and unanswerable queries overlap too much to separate
  (0.082 between the weakest real answer and the loudest nonsense), at 256 they
  don't (0.217). `GQLS_SEMANTIC_FLOOR` tunes it; `0` switches it off.
- A blank line separates an explanation's description from its annotations.
  Prose and a fact table were stacked at the same indent in the same weight, so
  a wrapped description's last line was indistinguishable from the first
  annotation. Only ever between two non-empty halves, and `-e` already
  separated its own sections this way.
- A description in a list is still one line, but the line is now what fits:
  elided on a word boundary to the width actually left after the columns, and
  dropped entirely when what's left is too narrow to tell one row from the next.
  It was elided at a fixed 72 columns regardless of the terminal, which
  overflowed. Naming a record shows the whole thing.
- An enum's values join the annotation table when they fit on one line, and
  become a block only when a value has something to say. `-D` and an
  undocumented enum both take the table form, where they previously printed an
  unpadded line that missed the label column and ran off the edge.
- A single result shows its whole description, as its own block indented under
  the row rather than hanging off the description column. The three-line cap
  exists so one documented row can't bury a list, and a lone result has no list
  to bury; wrapping the full text against a 60-column indent would turn it into
  a ribbon a few words wide.
- Descriptions wrap instead of running off the edge. A documented result ran to
  112 columns and the terminal broke it at column 0 — exactly where the next
  result's name starts, undoing the alignment. The text now wraps to the
  terminal's real width (measured directly; `$COLUMNS` isn't exported to child
  processes), indented under the description so a wrapped row reads as one
  block, and capped at three lines with the tail elided. Piped output uses a
  fixed 80 columns so a run is reproducible.
- The `-e` marker for an unexpanded object field is now `# posts: Post { … }`
  rather than `# posts: Post — add fields you need`. Same information, without
  repeating a sentence of English on every object-valued field, and `{ … }`
  shows the shape of what's missing. It stays a comment: there is no valid
  empty selection set, so `author { ... }`, `author { … }` and `author {}` are
  all parse errors, and a drafted operation has to survive a paste.
- Argument signatures collapse to `(…)` when a result sits alongside others,
  and are spelled out in full when it's the only match. Collapsing buys back
  the column width the longest signature would impose on every other row — 44
  columns against 22 on the example schema — and with a single result there are
  no other rows to protect, so `gqls Mutation.createUser` shows
  `(input: CreateUserInput!)`. `--json`/`--ndjson` are unchanged and always
  carry the full argument list.

### Fixed
- Running gqls with no query and nothing piped now says both ways to give it
  one. The piped branch — what CI, a script or an agent hits — only mentioned
  the pipe, so a reader who didn't know a positional query existed had no way
  to learn it from the error.
- Undescribed enum values no longer print trailing whitespace — and with colour
  on, the spaces were landing inside the escape wrapper.
- A deprecated enum value's marker is red, like every other deprecation. In a
  long enum the one value you must not use had been the least visible thing on
  screen.
- A schema whose `schema { … }` block carries a description now loads. The
  parser rejects the description and the whole file with it, pointing at
  `schema` and never mentioning the string above — so a schema that documents
  its own entry point failed entirely. Dropped before parsing, like the
  federation `extend schema` header already is; gqls builds no record for the
  schema definition, so nothing it would have shown is lost.
- Result columns line up. A record with no return type still paid for the
  separator `-> Type` would have used, so `[object]` sat one column off from
  every `[query]` and there was no vertical line for the eye to follow. Return
  type and kind are now real columns, measured on visible width, and a column
  nothing fills is dropped rather than left as a blank gutter.

## 0.21.0 — 2026-08-05

### Added
- Auto-discovery widens its search rather than giving up. A directory with no
  schema beneath it now falls back to the enclosing git repository, so `gqls
  user` inside `repo/src/components` finds the repo's schema instead of
  reporting that this particular subdirectory hasn't got one. Searching *down*
  from where you stand is still the rule — it's what lets a federated subgraph
  resolve to its own schema — and the fallback only happens where the answer
  was otherwise an error.
- When nothing turns up anywhere, a last pass searches the generated
  directories (`build`, `dist`, `target`, `tmp`, `coverage`) that are skipped
  on the way in, so a schema written by a build step is found rather than
  reported missing. Dependency directories (`node_modules`, `vendor`, `venv`)
  stay excluded even then: a schema in one describes someone else's API.

### Changed
- Embedding a query is ~2.5x faster (21.9ms to 8.7ms on a 10k-record schema),
  which takes a warm phrase query from ~119ms to ~108ms and compounds in batch
  mode, where every line pays it. ONNX Runtime was pinned to one thread — right
  for embedding a whole schema, where rayon already runs an inference per core,
  and wrong for embedding a single query, where the other cores sit idle. The
  session is now told which it's for. It has to be told rather than always
  taking the cores: an idle multi-threaded session spin-waits, and holding one
  during a whole-schema fill cost that fill 19%.
- A semantic query hashes the schema once rather than twice, and builds the
  text it hashes in parallel: 10ms to 4ms on a 10k-record schema. Deciding
  whether the vector cache is warm builds an embedding-text hash of every
  record; the session then built the same hash again to name the same file.
  It's computed once per run now and shown as its own `semantic: schema key`
  phase — it was previously unattributed, which is how it went unnoticed. In
  batch mode it was paid per query and is now paid once: five phrase queries
  against a 10k-record schema, 50ms of hashing to 4ms. The key itself is
  unchanged, so no cached vectors are orphaned.
- Schema auto-discovery is much faster, picking the same schema throughout.
  Sibling directories are searched in parallel; files are ruled out by name
  without building a path or allocating per entry; a candidate is only *read*
  when nothing already found outranks it, so a repo with a `schema.graphql`
  now opens no files at all; and the SDL sniff reads a bounded head rather
  than whole files. `venv`, `coverage` and `__pycache__` join the skipped
  directories. On a tree of 197 repos the search went 1,970ms to ~200ms
  (28,572 directories and 6,212 file reads, to 16,566 and none). A single
  monorepo was never dominated by the reads, so its ~30ms is unchanged —
  3,000 directories is what it costs — but see the next entry.
  The `.json` sniff still reads only the first 4KB: a real introspection dump
  names `__schema` at the top, and reading further only collects JSON that
  mentions it in passing.
- `-v`'s "N other schema file(s) elsewhere" now counts only candidates that
  were confirmed, which is all of them only when the search had to read
  everything. It can undercount; everything it names really is a schema.
- Discovery's answer is remembered per directory for an hour, so repeat runs
  skip the walk entirely: a warm query in that monorepo is 31ms to 5.6ms, and
  in the big tree 3.2s to 4.8ms. The remembered answer is dropped if the schema
  it names has moved, a directory with no schema is never remembered, and
  `--refresh` re-walks. `GQLS_DISCOVER_TTL` (seconds, `0` disables) tunes it.
- `-e` and `-R` act only on a field the query names — the name itself, or a
  small misspelling of it, in which case `Did you mean <path>?` heads the
  output and says which field was settled on. A looser query (`crtusr`,
  `User.`, a wildcard) now answers `Did you mean:` with the matches it found
  and exits 1, rather than drafting a paste-ready operation, or opening a
  file, for whatever ranked first. Both prompts go to stderr with the rest of
  gqls's own voice, so a draft still survives `> op.graphql` and JSON still
  survives `| jq`. Scripts that fed `-e`/`-R` an approximate name and used the result
  will now get a nonzero status and a candidate list. `-R` rejects before it
  shells out to rq.
- `-e` output is quieter and self-contained. The `drafting an operation for …`
  and `reached through …` stderr lines are now `-v`-only diagnostics — the
  draft already shows which root it nests through — and when several roots
  reach the target they're listed under the draft as a `# paths` block, the
  drafted one first, instead of on stderr. Sections with nothing in them are
  gone: no empty `# variables`, and no `# paths` for a single path the draft
  already shows. `# optional arguments, omitted above:` is now just
  `# optional arguments:`. `-j`/`--json` swaps the stderr-only root info for
  a `paths` array, so nothing is lost to a script.

### Fixed
- `--profile` accounts for the whole run. Schema auto-discovery — the tree walk
  that dominates a query when no source is passed — was not instrumented at
  all, so a report could show 10ms of phases under a 3.5s total and point the
  reader at everything except the problem. It's a phase now, and the report
  ends with an `unaccounted` line whenever the phases don't add up to the
  total, so the next gap announces itself.

## 0.20.0 — 2026-08-03

### Changed
- `--resolve` asks rq all its naming-convention candidates in one call rather
  than one process each. Same answers and the same wall clock — the previous
  version already ran them concurrently — but a fifth of the CPU (1.74s to
  0.32s over five lookups), since six processes were each opening the store,
  resolving the repo and checking the worktree to answer six one-line
  questions. Needs rq 0.38.0 or newer.

## 0.19.0 — 2026-08-03

### Added
- Queries piped on stdin, one per line, are answered by a single run — the
  schema, the embedding model and the vectors load once rather than once per
  query. 20 meaning-based queries against a 10k-record schema: 1.83s to 0.52s.
  Every row carries the `query` that produced it, and a query that matched
  nothing reports `{"query": …, "status": "no_matches"}` instead of vanishing
  from the stream. A single query's output is unchanged.

## 0.18.1 — 2026-08-03

### Fixed
- A failed vector-cache write no longer retires the files it was replacing. The
  new file was assumed to exist, so a write that failed — a full disk being the
  likeliest cause, and the one a large write provokes — could delete the only
  copies of a schema's vectors and force a full re-embed. Introduced in 0.18.0.
- An introspection response is validated before it's cached. A server answering
  `200` with an `errors` body (expired token, introspection disabled) had that
  response cached and replayed for the rest of the hour, turning a transient
  failure into a persistent one that only `--refresh` could clear.
- A cached introspection response that no longer parses is treated as a miss
  and refetched, rather than reported as an error until it expires. Responses
  are written via a temp file and renamed, so a reader never sees half of one.

### Changed
- Cached introspection responses are deleted after a week; nothing evicted them
  before, and they run to megabytes.
- The parsed-record cache keeps 8 files rather than 32. It has no consolidation,
  so an actively edited schema left a full copy per edit, and a miss only costs
  a re-parse.

## 0.18.0 — 2026-08-03

### Changed
- A schema edit now re-embeds **only what changed**. Vectors are keyed per
  record by the exact text they came from, so adding a few fields to a large
  schema costs a few inferences instead of re-embedding everything. On a
  10,319-record schema, a 151-record edit went from ~28s to ~0.5s. An unchanged
  schema still hits the cache whole, unaffected.
- The vector cache collects the files a write supersedes, so a schema that
  keeps gaining fields keeps one cache file rather than one per edit, and
  pruning is now bounded by total bytes (100MB) as well as file count. Going
  back to an older schema is still instant — its vectors live on in the file
  that replaced it.
- `--resolve`/`-R` runs its naming-convention candidates concurrently rather
  than one after another, so a lookup costs the slowest candidate instead of
  their sum. Results and ranking are unchanged.

### Upgrading
- Embedding vectors rebuild once on first use per schema (cache format
  `GQL1` → `GQL3`, which adds the per-record keys the above relies on). The
  rebuild runs in the background by default; `gqls --warm <schema>` does it up
  front.

## 0.17.0 — 2026-07-31
- Fuzzy search matches multi-word queries word by word, so a phrase is no
  longer a hard zero when the semantic index is cold or the build is
  fuzzy-only.
- Semantic search embeds words rather than camelCase spelling
  (`cancelSubscription` → `cancel Subscription`), which sharpens the separation
  between answerable and unanswerable queries at no cost.
- Record-cache decode roughly a third faster on warm queries.
- Vectors re-embed once on upgrade (the embedded text changed).

## 0.16.0 — 2026-07-30
- `--profile` prints a phase-by-phase timing breakdown; free when off.
- `script/bench.sh` for saving and diffing performance baselines.
- `-R` checks that a hit actually sits where the convention claimed.

## 0.15.0 — 2026-07-30
- `--example` emits fragments for an interface's implementors.
- `-R` ranking, truncation, and root-class fixes; guesses are labelled as such.

## 0.14.0 — 2026-07-30
- Inline fragments, `--depth`, and deprecation flags in drafted operations.

## 0.13.0 — 2026-07-30
- `--example` expands input types.

## 0.12.0 — 2026-07-30
- Typed placeholders, optional arguments, and argument defaults in `--example`.

## 0.11.0 — 2026-07-30
- `--example` drafts a runnable operation for a field.

## 0.10.0 — 2026-07-29
- Trailing-dot shorthand: `gqls User.` lists a type's members without quoting.

## 0.9.0 — 2026-07-29
- `--returns TYPE` filters to fields returning that type, wrappers peeled.

## 0.8.0 — 2026-07-29
- `?` (single character) and `{a,b}` (alternation) wildcards.

## 0.7.0 — 2026-07-29
- Wildcard queries enumerate matches instead of fuzzy-ranking them.

## 0.6.0 — 2026-07-28
- Descriptions in text output.

## 0.5.3 — 2026-07-28
- The semantic skip covers boundary-word matches (`name` → `lastName`).

## 0.5.2 — 2026-07-28
- Loose `Type name` queries, a tail bound on semantic results, tidier output.

## 0.5.1 — 2026-07-28
- Quieter default output.

## 0.5.0 — 2026-07-28
- Fixed a segfault on exit, skip semantic ranking on an exact match, widened
  the parse cache.

## 0.4.0 — 2026-07-28
- Ranking, caching, and parallelism work.

## 0.1.0 – 0.3.3 — 2026-07-27
- Initial releases: fuzzy and semantic search over SDL, introspection JSON, and
  live endpoints; `--resolve` field-to-resolver jump via
  [rq](https://github.com/dpep/rq); Apollo Federation v2 subgraph parsing with
  supergraph preference; shell completions; path-proximity ranking for
  monorepos; README, license, crate metadata, and the bundled Claude skill.
