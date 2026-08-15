# Changelog

Notable changes to `gqls`. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [SemVer](https://semver.org/).

Releases before 0.22.0 predate this file; the git tags are the record for those.

## Unreleased

### Changed

- **The published library API is a fifth of its former size** — 148 public
  items down to 30. `gqls` ships a library (as `gqls-cli` on crates.io)
  alongside the binary, and `lib.rs` re-exported all twelve modules, so most of
  that surface was public by default rather than by decision. `logging`,
  `paths`, `profile`, `render`, `resolve`, `semantic` and `style` are now
  crate-private, along with 25 items inside the five modules that consumers
  genuinely use.

  **No effect on the `gqls` binary or its behaviour.** This matters only if you
  depend on the library — `model::SchemaRecord`, `model::Kind`, `load::sdl`,
  `load::LoadOptions`, `search::search`, `search::Hit`, `search::Filters`,
  `example::build` and `cli::run` remain public; most other paths do not.

### Removed

- `semantic::search`, a one-shot convenience wrapper, was never called — the
  CLI builds its own `Session`. It had been dead since the initial commit and
  was invisible to the `dead_code` lint only because `semantic` was a public
  module.

### Fixed

- `paths::temp_dir` is now gated on the `_semantic` feature, matching its only
  caller. It was compiled, and dead, in the fuzzy-only build.

### Internal

- `unreachable_pub` is on. A `pub` item inside a private module is reachable
  from nowhere, and `pub` is exactly what makes the `dead_code` lint skip an
  item — so the two together were hiding unused code, as the two entries above
  show.
