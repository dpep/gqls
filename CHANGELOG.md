# Changelog

Notable changes to `gqls`. The CLI surface — flags, output shape, exit codes —
is the public API; the crate is not intended to be used as a library.

Versions before 0.18.0 are reconstructed from release commits and tags, so the
early entries are terser than what follows.

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
