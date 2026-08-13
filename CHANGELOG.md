# Changelog

Notable changes to `gqls`. The CLI surface — flags, output shape, exit codes —
is the public API; the crate is not intended to be used as a library.

Versions before 0.18.0 are reconstructed from release commits and tags, so the
early entries are terser than what follows.

## Unreleased

### Added
- Colour in text output: the matched path bold, everything else — arguments,
  the arrow, the return type, the kind tag, the description — dimmed. Two
  weights and no hue, so nothing can clash with a terminal theme; bold and dim
  are relative to your own foreground, where any colour is a guess about your
  palette. Suppressed when stdout isn't a TTY or `NO_COLOR` is set, and it only
  ever adds escapes, so the visible characters are identical either way.

### Added
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

### Added
- `-e` leads with what the field does, as a comment above the operation. It has
  already committed to one field — it refuses to draft unless the query named
  it — so the whole description goes in, wrapped and uncapped.

### Fixed
- A schema whose `schema { … }` block carries a description now loads. The
  parser rejects the description and the whole file with it, pointing at
  `schema` and never mentioning the string above — so a schema that documents
  its own entry point failed entirely. Dropped before parsing, like the
  federation `extend schema` header already is; gqls builds no record for the
  schema definition, so nothing it would have shown is lost.

### Changed
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
