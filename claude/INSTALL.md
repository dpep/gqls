# Installing the gqls skill

Two separate things: the **binary**, which does the work, and the **skill**,
which teaches Claude to drive it. You need both, and they install independently.

## The binary

```sh
brew install dpep/tools/gqls    # macOS/Homebrew — includes semantic search
```

No Homebrew:

```sh
cargo install gqls-cli                          # semantic search included
cargo install gqls-cli --no-default-features    # lean, fuzzy-only
```

Update with `brew upgrade dpep/tools/gqls`, or re-run the `cargo install` line.

## The skill

Two routes. They suit different people, so ask rather than picking.

### The marketplace plugin (the better default)

```
/plugin marketplace add dpep/claude
/plugin install code@dpep
```

One install, and `claude plugin update code@dpep` keeps it current.

Prefer this unless there's a reason not to. A skill file describing an older
binary than the one installed is the failure mode worth avoiding, and this is
the route that gets updates.

### A local copy

```sh
mkdir -p ~/.claude/skills/gqls
cp claude/gqls-skill.md ~/.claude/skills/gqls/SKILL.md
```

Just this skill, nothing else — right when the user wants nothing else from the marketplace.

Either way, restart Claude Code after installation.
