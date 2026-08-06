#!/usr/bin/env bash
#
# Cut a release: bump, gate, tag, publish, brew, release page, skills.
#
#     script/release.sh 0.22.0
#     script/release.sh patch --summary "discovery stopped being the slow part"
#     script/release.sh minor --dry-run
#
# Why a script: this chain is nineteen steps and has been hand-run nine times
# in three days. Every step worked every time, which is the argument — the
# risk isn't that a step is hard, it's that step fourteen gets forgotten at
# the end of a long session and a channel silently ships stale. Twice the
# thing forgotten was the skill copy, which no test covers.
#
# Every step asks whether it has already happened and skips if so, so a run
# that dies halfway — a network blip mid-publish, a formula that fails audit —
# is re-run with the same arguments and picks up where it stopped. Nothing
# here is written to be run twice in anger, but it will be.
#
# The irreversible steps are `cargo publish` and the pushes. They come after
# the gate and after the version exists as a tag, so the ordering is: make it
# true locally, prove it, then tell the world.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

TAP_DIR="${GQLS_TAP_DIR:-$(brew --repository 2>/dev/null)/Library/Taps/dpep/homebrew-tools}"
SKILL_SRC="claude/gqls-skill.md"
SKILL_DST="${GQLS_SKILL_DST:-$HOME/code/lib/claude/plugins/code/skills/gqls/SKILL.md}"
SKILL_REPO="$(dirname "$SKILL_DST")"
CRATE="gqls-cli"

SUMMARY=""
DRY_RUN=false
VERSION_ARG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --summary) SUMMARY="${2:?--summary needs a value}"; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) VERSION_ARG="$1"; shift ;;
  esac
done

step()  { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
skip()  { printf '    (already done: %s)\n' "$*"; }
die()   { printf '\033[31mrelease: %s\033[0m\n' "$*" >&2; exit 1; }
run()   { if $DRY_RUN; then printf '    would run: %s\n' "$*"; else "$@"; fi; }

CURRENT="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[ -n "$CURRENT" ] || die "can't read the current version from Cargo.toml"

# --- version -----------------------------------------------------------------

case "${VERSION_ARG:-}" in
  major|minor|patch)
    VERSION="$(python3 - "$CURRENT" "$VERSION_ARG" <<'PY'
import sys
major, minor, patch = (int(p) for p in sys.argv[1].split("."))
bump = sys.argv[2]
if bump == "major":
    major, minor, patch = major + 1, 0, 0
elif bump == "minor":
    minor, patch = minor + 1, 0
else:
    patch += 1
print(f"{major}.{minor}.{patch}")
PY
)" ;;
  [0-9]*.[0-9]*.[0-9]*) VERSION="$VERSION_ARG" ;;
  "") die "usage: script/release.sh <version | major | minor | patch> [--summary TEXT] [--dry-run]" ;;
  *) die "not a version or a bump: $VERSION_ARG" ;;
esac

TAG="v$VERSION"
TODAY="$(date +%Y-%m-%d)"
echo "releasing $CURRENT -> $VERSION${DRY_RUN:+ (dry run)}"

# --- preflight ---------------------------------------------------------------
# Everything that would be annoying to discover at step twelve.

step "preflight"
[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] || die "not on main"
if ! git diff --quiet || ! git diff --cached --quiet; then
  die "working tree is dirty"
fi
git fetch --quiet origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] ||
  die "main and origin/main have diverged — push or pull first"
grep -q '^## Unreleased' CHANGELOG.md ||
  die "CHANGELOG.md has no '## Unreleased' section to release"
python3 - <<'PY' || die "the '## Unreleased' section is empty — nothing to release"
import re, sys
s = open("CHANGELOG.md").read()
body = re.split(r"^## ", s, flags=re.M)[1]
sys.exit(0 if body.splitlines()[1:] and any(l.strip() for l in body.splitlines()[1:]) else 1)
PY
[ -d "$TAP_DIR" ] || die "homebrew tap not found at $TAP_DIR (set GQLS_TAP_DIR)"
[ -f "$SKILL_DST" ] || die "plugin skill copy not found at $SKILL_DST (set GQLS_SKILL_DST)"
command -v gh >/dev/null || die "gh is not installed"
echo "    on main, clean, in sync; changelog has content; tap and skill found"

# --- bump --------------------------------------------------------------------

step "bump to $VERSION"
if [ "$CURRENT" = "$VERSION" ]; then
  skip "Cargo.toml is already $VERSION"
elif $DRY_RUN; then
  echo "    would set version = \"$VERSION\" in Cargo.toml and update Cargo.lock"
else
  python3 - "$CURRENT" "$VERSION" <<'PY'
import sys
old, new = sys.argv[1], sys.argv[2]
p = "Cargo.toml"
s = open(p).read().replace(f'version = "{old}"', f'version = "{new}"', 1)
open(p, "w").write(s)
PY
  cargo update --package "$CRATE" --precise "$VERSION" >/dev/null
fi

step "changelog"
if grep -q "^## $VERSION " CHANGELOG.md; then
  skip "CHANGELOG.md already has a $VERSION heading"
elif $DRY_RUN; then
  echo "    would retitle '## Unreleased' as '## $VERSION — $TODAY'"
else
  python3 - "$VERSION" "$TODAY" <<'PY'
import sys
version, today = sys.argv[1], sys.argv[2]
p = "CHANGELOG.md"
s = open(p).read().replace("## Unreleased", f"## {version} — {today}", 1)
open(p, "w").write(s)
PY
fi

# --- prove it ----------------------------------------------------------------

step "gate"
if $DRY_RUN; then
  echo "    would run script/check.sh"
else
  script/check.sh
fi

# --- commit, tag, push -------------------------------------------------------

step "commit and tag $TAG"
SUBJECT="Release $VERSION"
if [ -n "$SUMMARY" ]; then
  SUBJECT="Release $VERSION ($SUMMARY)"
fi
if $DRY_RUN; then
  # The bump above didn't really happen, so there's nothing staged to see.
  echo "    would commit: $SUBJECT"
elif git diff --quiet && git diff --cached --quiet; then
  skip "nothing to commit"
else
  git add Cargo.toml Cargo.lock CHANGELOG.md
  git commit -F - <<EOF
$SUBJECT

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  skip "tag $TAG exists"
else
  run git tag "$TAG"
fi

step "push"
run git push origin main
run git push origin "$TAG"

# --- publish -----------------------------------------------------------------

step "cargo publish"
# crates.io answers 403 to a request with no User-Agent, which read as "not
# published" and would send a resumed run back into `cargo publish` — an error,
# not a no-op, so the run would die at the step it had already completed.
if curl -fsS -A "gqls-release-script (github.com/dpep/gqls)" \
  "https://crates.io/api/v1/crates/$CRATE/$VERSION" >/dev/null 2>&1; then
  skip "$CRATE $VERSION is on crates.io"
else
  run cargo publish
fi

# --- homebrew ----------------------------------------------------------------

step "homebrew formula"
FORMULA="$TAP_DIR/Formula/gqls.rb"
[ -f "$FORMULA" ] || die "formula not found at $FORMULA"
if grep -q "$TAG.tar.gz" "$FORMULA"; then
  skip "formula points at $TAG"
elif $DRY_RUN; then
  echo "    would fetch the $TAG tarball, compute its sha256, and update $FORMULA"
else
  TARBALL="$(mktemp -t gqls-release)"
  # The tag has to exist on GitHub for this to resolve, which is why the push
  # comes first. A 404 here means the push didn't land, not that the sha moved.
  curl -fsSL "https://github.com/dpep/gqls/archive/refs/tags/$TAG.tar.gz" -o "$TARBALL"
  SHA="$(shasum -a 256 "$TARBALL" | cut -d' ' -f1)"
  rm -f "$TARBALL"
  echo "    sha256 $SHA"
  python3 - "$FORMULA" "$TAG" "$SHA" <<'PY'
import re, sys
formula, tag, sha = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(formula).read()
s = re.sub(r"/tags/v[0-9.]+\.tar\.gz", f"/tags/{tag}.tar.gz", s)
s = re.sub(r'sha256 "[0-9a-f]{64}"', f'sha256 "{sha}"', s, count=1)
open(formula, "w").write(s)
PY
fi

step "brew build, test, audit"
if brew list --versions gqls 2>/dev/null | grep -q "^gqls $VERSION$"; then
  skip "gqls $VERSION is installed"
else
  run brew uninstall gqls
  run brew install --build-from-source dpep/tools/gqls
fi
run brew test dpep/tools/gqls
run brew audit --strict --online dpep/tools/gqls

step "push tap"
if git -C "$TAP_DIR" diff --quiet -- Formula/gqls.rb; then
  skip "tap has no formula change to push"
else
  run git -C "$TAP_DIR" add Formula/gqls.rb
  if ! $DRY_RUN; then
    git -C "$TAP_DIR" commit -F - <<EOF
gqls $VERSION

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
  fi
  run git -C "$TAP_DIR" push
fi

# --- release page ------------------------------------------------------------

step "github release"
if gh release view "$TAG" >/dev/null 2>&1; then
  skip "release $TAG exists"
elif $DRY_RUN; then
  echo "    would create release $TAG from the changelog section"
else
  NOTES="$(mktemp -t gqls-notes)"
  python3 - "$VERSION" > "$NOTES" <<'PY'
import re, sys
version = sys.argv[1]
s = open("CHANGELOG.md").read()
start = s.index(f"## {version} ")
rest = s[start:]
end = rest.index("\n## ", 1)
print(rest[:end].split("\n", 1)[1].strip())
PY
  TITLE="$TAG"
  if [ -n "$SUMMARY" ]; then
    TITLE="$TAG — $SUMMARY"
  fi
  gh release create "$TAG" --title "$TITLE" --notes-file "$NOTES"
  rm -f "$NOTES"
fi

# --- skills ------------------------------------------------------------------
# The step most likely to be skipped by hand, and the one nothing else catches:
# a stale skill misinforms an agent for a whole release cycle, silently.

step "sync skill"
if $DRY_RUN; then
  echo "    would sync $SKILL_SRC -> $SKILL_DST"
else
  python3 - "$SKILL_SRC" "$SKILL_DST" <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src).read()
# The install footer addresses someone reading the repo, not the installed skill.
marker = "\nTo install this skill for Claude Code, copy it to"
if marker in s:
    s = s[: s.index(marker)].rstrip() + "\n"
open(dst, "w").write(s)
PY
fi
if git -C "$SKILL_REPO" diff --quiet; then
  skip "plugin skill copy is current"
else
  run git -C "$SKILL_REPO" add -A
  if ! $DRY_RUN; then
    git -C "$SKILL_REPO" commit -F - <<EOF
gqls skill: $VERSION

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
  fi
  run git -C "$SKILL_REPO" push
fi

# --- done --------------------------------------------------------------------

step "released $VERSION"
cat <<EOF
  crates.io   https://crates.io/crates/$CRATE/$VERSION
  release     https://github.com/dpep/gqls/releases/tag/$TAG
  brew        $(brew list --versions gqls 2>/dev/null || echo 'not installed')

  Worth a look before you walk away:
    gqls --version        the shipped binary, not the dev build
EOF
