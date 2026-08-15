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

One install, and `claude plugin update code@dpep` keeps it current. It brings
the `rq`, `git`, `find-skill` and `find-gem` skills along with it, which is
either the point or the objection depending on the person.

Prefer this unless there's a reason not to. A skill file describing an older
binary than the one installed is the failure mode worth avoiding, and this is
the route that gets updates.

### A local copy

```sh
mkdir -p ~/.claude/skills/gqls
cp claude/gqls-skill.md ~/.claude/skills/gqls/SKILL.md
```

Just this skill, nothing else — and it stops tracking upstream the moment it
lands. Right when the user wants nothing else from the marketplace, is offline,
or is trying a modified version of the skill.

Either way, restart Claude Code before it loads.

## For whoever maintains this

`claude/gqls-skill.md` is the source. `script/release.sh` copies it verbatim
into the marketplace at release; the two files are meant to be byte-identical,
which is why this document is separate rather than a section inside the skill —
a sync that transforms its input is a sync that can be wrong about whether it
ran.
