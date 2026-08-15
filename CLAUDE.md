# gqls

Fuzzy and semantic search over a GraphQL schema. Everything flattens to a
`SchemaRecord`; search and output touch nothing else. See the README's "How it
works" for the layering.

## What gqls is for

**gqls answers questions about a GraphQL schema.** Searching is how you reach
the answer, not the answer itself. So: narrow fast, and once the question
resolves to one thing, say everything known about it.

That's the frame the rest of the tool hangs off. Ranking, the fuzzy/semantic
combine and the wildcards all serve the first half; the annotations on a named
record — its description in full, its deprecation reason, its directives, an
abstract type's members, an enum's values, what references a type — serve the
second. `-e` sits outside both on purpose: drafting an operation answers *how do
I call this*, which is a different question rather than a fuller answer to *what
is this*.

The litmus test for anything new:

> Does it help someone **find** something in a schema, or **understand**
> something they found? If neither, it doesn't belong in gqls.

It's a real filter, not a slogan. A `--bare` flag printing only the drafted
operation failed it — that's output plumbing, and `-j | jq -r .operation`
already did it. Annotating a named record passed it on the second half, which
is why it's in the base case and not behind a flag.

## Scripts — use these, don't hand-run their steps

- **`script/check.sh`** — the gate. Formatting, clippy, and tests at every
  feature configuration gqls ships. Run before every commit or push. It cleans
  this crate first, because cargo's fingerprint wedges "fresh" here and will
  otherwise validate code you didn't write (`cargo clean -p gqls-cli` is the
  manual fix if a build reports an error that contradicts the source).
- **`script/release.sh <version | major | minor | patch>`** — the whole release:
  bump, changelog heading, gate, tag, push, `cargo publish`, Homebrew formula
  (sha, build, test, audit, tap push), the GitHub release page, and the plugin
  skill copy. Use it rather than the nineteen steps by hand; `--dry-run` first
  if unsure. Every step skips what's already done, so an interrupted run is
  re-run with the same arguments.
- **`script/bench.sh`** — the performance baseline. `--save NAME` / `--diff NAME`.

## Two things that have burned this repo

- **A perf claim from a single run is usually noise.** Warm-cache and
  machine-load variance here runs ±30%; measured numbers came out 149ms one
  hour and 198ms the next for identical code. Use `script/bench.sh`, or min of
  N runs, before saying something got faster.
- **Don't filter tool output while diagnosing.** `| tail`, `| grep` for one
  error code, and a truncated result list have each hidden the evidence that
  would have disproved the hypothesis. Re-run unfiltered before concluding, and
  check which code path produced a number before interpreting it.

## Changelog

Add the entry under `## Unreleased` in the same change that earns it — the
release script turns that heading into the version and the release notes, and
refuses to run if it's missing or empty.

## The skill has three copies; keep them one

`claude/gqls-skill.md` is the source. It is copied verbatim — no edits, no
stripping — to:

- `~/code/lib/claude/plugins/code/skills/gqls/SKILL.md`, the public marketplace
- whatever a user installed, which updates only when the **code plugin's
  version** moves in `plugins/code/.claude-plugin/plugin.json`

`script/release.sh` does both at release time. **When you change the skill
outside a release, copy it to the marketplace in the same commit and bump the
code plugin's minor version**, or the change reaches nobody: `claude plugin
update` compares versions, not content, and will report a plugin current while
it serves the old file. That has already happened once — four skill-touching
commits shipped under one plugin version.

Install guidance for humans lives in `claude/INSTALL.md`, deliberately outside
the skill so the copy stays a copy.
