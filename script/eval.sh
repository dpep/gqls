#!/usr/bin/env bash
#
# A ranking-quality measurement: hit@1, hit@5 and MRR against labelled
# query -> expected-record pairs.
#
#     script/eval.sh              # run every set, print a report
#     script/eval.sh --json       # emit raw per-query results
#     script/eval.sh --save NAME  # store a baseline
#     script/eval.sh --diff NAME  # compare against one
#
# Why a script: every ranking judgement so far has been a spot check or a
# differential against an older binary, which shows what changed but never
# whether it got better. This fixes the query sets so a ranking change is a
# measured diff instead of a memory. Run it before and after any change to
# ranking, fuzzy matching or the score combine.
#
# Two labelled sets, same TSV format (category<TAB>query<TAB>expected path):
#   script/eval/examples.tsv - ~12 queries against the repo's own
#     examples/schema.graphql, no network needed. Categories: N (exact name),
#     A (abbreviation), T (typo), Q (qualified Type.field), P (phrase),
#     G (root field reached by an argument name).
#   script/eval/github.tsv   - 49 queries against GitHub's public schema,
#     fetched on demand into a cache dir and never checked in (1.5MB).
#     Categories: V (vocabulary mismatch - the query shares no word with the
#     answer), O (word overlap). Skipped with a clear message when the
#     schema is absent and the network is down.
#
# This is a measurement, not a gate: it exits nonzero only on a harness
# failure (missing binary, no set could run), never for a low score. Fewer
# than a hundred labelled queries is a small sample - read a change here as
# "better" or "worse", not as a precise percentage.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

EVAL_DIR="${TMPDIR:-/tmp}/gqls-eval"
BASELINES="$EVAL_DIR/baselines"
GITHUB_SCHEMA="$EVAL_DIR/github.graphql"
GITHUB_SCHEMA_URL="https://raw.githubusercontent.com/octokit/graphql-schema/main/schema.graphql"
GQLS="target/release/gqls"
# Comfortably past the CLI's own default of 20: a rank the eval can't see is
# indistinguishable from "never matched", so the cutoff should be one nobody
# scrolls to in practice, not the one gqls shows by default.
LIMIT=50

mkdir -p "$BASELINES"

[ -x "$GQLS" ] || { echo "build first: cargo build --release" >&2; exit 1; }

if [ ! -f "$GITHUB_SCHEMA" ]; then
  echo "fetching GitHub's schema (cached after this, at $GITHUB_SCHEMA)…" >&2
  if curl -fsSL -o "$GITHUB_SCHEMA.tmp" "$GITHUB_SCHEMA_URL"; then
    mv "$GITHUB_SCHEMA.tmp" "$GITHUB_SCHEMA"
  else
    rm -f "$GITHUB_SCHEMA.tmp"
    echo "no network and no cached schema - skipping script/eval/github.tsv" >&2
  fi
fi

SETS="examples script/eval/examples.tsv examples/schema.graphql"$'\n'
if [ -f "$GITHUB_SCHEMA" ]; then
  SETS+="github script/eval/github.tsv $GITHUB_SCHEMA"$'\n'
fi

# One labelled set's queries through a single batched gqls process (`-J` on
# piped stdin - one process for the whole set, not one per query, since
# that's ~2x faster and the documented way). Emits one JSON line per labelled
# query: {set, category, query, target, rank}, rank null when the target
# never appeared in the top LIMIT.
# A schema's descriptions can carry backticks and `$` (markdown code spans,
# shell examples), which would misfire as command substitution if piped
# straight into an unquoted heredoc — so gqls's own output goes to a temp
# file, and only our own controlled {set, category, query, target} fields
# ever get embedded into a heredoc below.
run_set() {
  local name="$1" tsv="$2" schema="$3"
  local tmp
  tmp="$(mktemp "$EVAL_DIR/run.XXXXXX")"
  cut -f2 "$tsv" | "$GQLS" "$schema" -J --no-explain -l "$LIMIT" -q 2>/dev/null > "$tmp"
  python3 - "$name" "$tsv" "$tmp" <<'PY'
import json, sys
name, tsv, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
rows = [l.rstrip("\n").split("\t") for l in open(tsv) if l.strip()]

# Correlate by order, not by the echoed `query`: gqls echoes the query as
# searched, so `user followers` comes back as `user.followers` (the two-word
# form rewritten to the qualified one) and matching on the string scores
# every rewritten query as a miss. Each query's rows are contiguous and every
# query emits at least one row — a miss emits `status: no_matches` — so the
# groups line up with the input lines one for one.
groups, current = [], None
for line in open(out_path):
    if not line.strip():
        continue
    d = json.loads(line)
    if d["query"] != current:
        current = d["query"]
        groups.append([])
    groups[-1].append(d.get("path"))
if len(groups) != len(rows):
    sys.exit(f"eval: {len(groups)} answered groups for {len(rows)} queries in {tsv}")
for (cat, q, target), paths in zip(rows, groups):
    rank = paths.index(target) + 1 if target in paths else None
    print(json.dumps({"set": name, "category": cat, "query": q, "target": target, "rank": rank}))
PY
  rm -f "$tmp"
}

raw=""
while IFS=' ' read -r name tsv schema; do
  [ -z "$name" ] && continue
  raw+="$(run_set "$name" "$tsv" "$schema")"$'\n'
done <<<"$SETS"

case "${1:-}" in
  --json) printf '%s' "$raw" ;;
  --save)
    printf '%s' "$raw" > "$BASELINES/${2:?name required}.jsonl"
    echo "saved baseline '${2}'" >&2
    ;;
  --diff)
    python3 - "$BASELINES/${2:?name required}.jsonl" <<PY
import json, sys
from collections import defaultdict

def load(lines):
    rows = [json.loads(l) for l in lines if l.strip()]
    return rows

def summarize(rows):
    # (set, category) -> ranks, grouped by set (first-seen order) with
    # "overall" appended last within each set.
    by_set = defaultdict(list)
    for r in rows:
        by_set[r["set"]].append(r)
    ranks_by_key = {}
    for set_name, set_rows in by_set.items():
        by_cat = defaultdict(list)
        for r in set_rows:
            by_cat[r["category"]].append(r["rank"])
        by_cat["overall"] = [r["rank"] for r in set_rows]
        for cat, ranks in by_cat.items():
            ranks_by_key[(set_name, cat)] = ranks
    out = {}
    for key, ranks in ranks_by_key.items():
        n = len(ranks)
        hit1 = sum(1 for x in ranks if x == 1) / n
        hit5 = sum(1 for x in ranks if x and x <= 5) / n
        mrr = sum(1 / x for x in ranks if x) / n
        out[key] = (round(hit1, 2), round(hit5, 2), round(mrr, 2))
    return out

base = summarize(load(open(sys.argv[1])))
now = summarize(load("""$raw""".splitlines()))

def fmt(v):
    return f"{v:.2f}"

print(f"{'set/category':<20}{'hit@1':>20}{'hit@5':>20}{'MRR':>20}")
for key in now:
    b, n = base.get(key), now[key]
    label = "/".join(key)
    if b is None:
        cells = [f"{'—':>9}->{fmt(v):<9}" for v in n]
    else:
        cells = [f"{fmt(bv):>9}->{fmt(nv):<9}" for bv, nv in zip(b, n)]
    print(f"{label:<20}" + "".join(f"{c:>20}" for c in cells))
PY
    ;;
  *)
    python3 - <<PY
import json
from collections import defaultdict

rows = [json.loads(l) for l in """$raw""".splitlines() if l.strip()]
by_set = defaultdict(list)
for r in rows:
    by_set[r["set"]].append(r)

for set_name, set_rows in by_set.items():
    print(f"\n=== {set_name} (n={len(set_rows)}) ===")
    by_cat = defaultdict(list)
    for r in set_rows:
        by_cat[r["category"]].append(r["rank"])
    by_cat["overall"] = [r["rank"] for r in set_rows]
    print(f"{'category':<10}{'hit@1':>8}{'hit@5':>8}{'MRR':>8}")
    for cat, ranks in by_cat.items():
        n = len(ranks)
        hit1 = round(sum(1 for x in ranks if x == 1) / n, 2)
        hit5 = round(sum(1 for x in ranks if x and x <= 5) / n, 2)
        mrr = round(sum(1 / x for x in ranks if x) / n, 2)
        print(f"{cat:<10}{hit1:>8.2f}{hit5:>8.2f}{mrr:>8.2f}")
    misses = [r for r in set_rows if r["rank"] != 1]
    if misses:
        print("\nmisses (rank the expected record actually got):")
        for r in misses:
            rank = r["rank"] if r["rank"] is not None else "not in top $LIMIT"
            print(f"  [{r['category']}] {r['query']!r} -> {r['target']}  rank={rank}")
PY
    ;;
esac
