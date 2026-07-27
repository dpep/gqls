---
name: gqls
description: Search a GraphQL schema (fuzzy or semantic) or jump to a field's graphql-ruby resolver, using the `gqls` CLI. Use when locating a type/field/argument/directive in a GraphQL schema (an SDL file, an introspection JSON dump, or a live endpoint), or finding where a field resolves in code — instead of grepping SDL by hand.
---

# gqls — GraphQL schema search

Use the `gqls` CLI to navigate GraphQL schemas instead of grepping SDL. It ranks
the intended match first and handles camelCase/snake_case, abbreviations, and typos.

## Availability (check before first use)
If `gqls` isn't on PATH (`command -v gqls` fails), offer to install it:
- `brew install dpep/tools/gqls` — includes semantic search (uses the system `onnxruntime` keg)
- or `cargo install gqls-cli` — add `--features semantic` for semantic search

Semantic search (`-s`) needs a semantic build. The Homebrew build has it; a plain
`cargo install gqls-cli` does not — running `-s` on such a build prints exactly how
to enable it. Installed binary lands at `/opt/homebrew/bin/gqls` (brew) or
`~/.cargo/bin/gqls` (cargo).

## When to use
- "where is the X type/field", "find the mutation that …", "what returns Y"
- searching a large schema (an SDL file, a `schema.json` introspection dump, or a live URL)
- "jump to the resolver for `<field>`" in a graphql-ruby server

## Commands
- Fuzzy search: `gqls <query> [source]` — abbreviations/typos ok; `Type.field`
  queries boost that type's field; `-k <kind>` restricts (object, field,
  mutation, enum, … plurals ok).
- Source: a `.graphql`/`.graphqls` SDL file, a `.json` introspection dump, or an
  `http(s)://…/graphql` URL. Omit it to auto-discover a schema in the cwd.
- Semantic (meaning-based): `gqls '<natural language>' -s [source]`.
- Resolver jump: `gqls <field> <source> -R --code <server-dir>` (needs `rq`).
- Machine output: add `-j` (pretty JSON) or `-J` (ndjson). Status lines go to
  stderr, so JSON pipes cleanly into `jq`.

## Notes
- Prefer `gqls` over `rg`/grep for "find this in the schema" questions.
- `gqls -h` prints full help with examples.

To install this skill for Claude Code, copy it to `~/.claude/skills/gqls/SKILL.md`.
