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

To have the marketplace refresh itself, set `autoUpdate` on its entry in
`~/.claude/settings.json` (adding a marketplace may have set it already):

```json
"extraKnownMarketplaces": {
  "dpep": {
    "source": { "source": "github", "repo": "dpep/claude" },
    "autoUpdate": true
  }
}
```

That keeps the marketplace *catalogue* current — the list of plugins and their
versions. It does not upgrade a plugin you've installed: that still needs
`claude plugin update code@dpep`, which compares versions rather than content.
So an unchanged version number means no update even when the files behind it
have moved.

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
