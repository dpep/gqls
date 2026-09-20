#!/usr/bin/env bash
#
# A ranking-quality measurement: hit@1, hit@5 and MRR against labelled
# query -> expected-record pairs.
#
#     script/eval.sh              # run every set, print a report
#     script/eval.sh --json       # emit raw per-query results
#     script/eval.sh --save NAME  # store a baseline
#     script/eval.sh --diff NAME  # compare against one, with per-query movement
#     script/eval.sh --selftest   # check the harness itself, not ranking
#
# Why a script: every ranking judgement so far has been a spot check or a
# differential against an older binary, which shows what changed but never
# whether it got better. This fixes the query sets so a ranking change is a
# measured diff instead of a memory. Run it before and after any change to
# ranking, fuzzy matching or the score combine.
#
# Four labelled sets, same TSV format (category<TAB>query<TAB>expected path):
#   script/eval/examples.tsv - ~12 queries against the repo's own
#     examples/schema.graphql, no network needed. Categories: N (exact name),
#     A (abbreviation), T (typo), Q (qualified Type.field), P (phrase),
#     G (root field reached by an argument name).
#   script/eval/github.tsv   - 49 queries against GitHub's public schema,
#     fetched on demand into a cache dir and never checked in (1.5MB).
#     Categories: V (vocabulary mismatch - the query shares no word with the
#     answer), O (word overlap). Skipped with a clear message when the
#     schema is absent and the network is down.
#   script/eval/holdout.tsv  - ~20 queries against AniList's public schema
#     (https://graphql.anilist.co), introspected on demand into the same
#     cache dir and never checked in. This is the set to NOT tune against:
#     it shares no vocabulary or schema authorship with the other two, so a
#     ranking change that only memorizes examples.tsv/github.tsv shows up
#     here as flat while the tuning sets improve. The report labels it
#     accordingly - read its numbers as "did this generalize", not as a
#     target to chase.
#   script/eval/hasura.tsv   - 20 queries against PokeAPI's public Hasura
#     schema (https://beta.pokeapi.co/graphql/v1beta), introspected on demand
#     into the same cache dir and never checked in. Unlike holdout, this one
#     is ADVERSARIAL, not held out - it's fair game for tuning, since it
#     targets a known-weak surface on purpose: every record shares the
#     `pokemon_v2_` prefix, and every description that exists at all (7062 of
#     26218 records) is a machine-generated template ("fetch data from the
#     table: X") that just echoes the record's own name, carrying no
#     vocabulary a name doesn't already have. Categories: O (word overlap -
#     the query reads as the record's name in English word order), V
#     (vocabulary mismatch - a content word has no counterpart in the name,
#     and the boilerplate description can't supply one either), S
#     (suffix-sibling disambiguation - the base table, `_aggregate` and
#     `_by_pk` variants share one identical stem, so only the query's intent,
#     not its words, picks the right sibling). Skipped with a clear message
#     when absent and unreachable, same as the other two live sets.
#
# This is a measurement, not a gate: it exits nonzero only on a harness
# failure (missing binary, no set could run, a self-check trips), never for a
# low score. Fewer than a hundred labelled queries is a small sample - read a
# change here as "better" or "worse", not as a precise percentage.
#
# --selftest and the always-on identity check below exist because this
# harness has already shipped two bugs that produced a plausible-looking
# report: a heredoc silently ate a pipe so every rank came back null, and
# (before that) rows were correlated by the echoed `query` field, which gqls
# rewrites, so every rewritten query scored as a miss. Both would trip these
# checks - see `selftest()`.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

EVAL_DIR="${TMPDIR:-/tmp}/gqls-eval"
BASELINES="$EVAL_DIR/baselines"
GITHUB_SCHEMA="$EVAL_DIR/github.graphql"
GITHUB_SCHEMA_URL="https://raw.githubusercontent.com/octokit/graphql-schema/main/schema.graphql"
HOLDOUT_SCHEMA="$EVAL_DIR/anilist.json"
HOLDOUT_URL="https://graphql.anilist.co"
HASURA_SCHEMA="$EVAL_DIR/hasura.json"
HASURA_URL="https://beta.pokeapi.co/graphql/v1beta"
GQLS="target/release/gqls"
# Comfortably past the CLI's own default of 20: a rank the eval can't see is
# indistinguishable from "never matched", so the cutoff should be one nobody
# scrolls to in practice, not the one gqls shows by default.
LIMIT=50

mkdir -p "$BASELINES"

[ -x "$GQLS" ] || { echo "build first: cargo build --release" >&2; exit 1; }

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
  # Capture the heredoc's exit status before `rm` overwrites $? with its own -
  # the count-mismatch guard above is a `sys.exit`, and without this a
  # trailing cleanup command silently absorbs the failure so `set -e` never
  # sees it and the script carries on with a partial report.
  local status=$?
  rm -f "$tmp"
  return "$status"
}

# A self-check that would have caught both bugs described up top, run against
# only the offline examples set so it needs no network and no binary state
# beyond the release build.
#
#   1. Every row in examples.tsv where the query text IS the expected path
#      (the `N`/`Q` rows: `Comment` -> `Comment`, `Post.author` ->
#      `Post.author`) is the easiest possible match there is. If one doesn't
#      rank 1, the plumbing broke, not the ranking — this is what "every rank
#      came back null" looks like.
#   2. A query built from shell metacharacters (backtick, `$(...)`) round-trips
#      through the exact same temp-file-then-heredoc path real schema
#      descriptions use, and must come back byte-for-byte and still rank 1 —
#      this is what a careless "just interpolate the string" fix for bug 2
#      would have broken.
selftest() {
  echo "selftest: identity queries (query text == expected path) rank 1..." >&2
  local out identity_tmp
  out="$(run_set examples script/eval/examples.tsv examples/schema.graphql)"
  identity_tmp="$(mktemp "$EVAL_DIR/selftest-identity.XXXXXX")"
  printf '%s\n' "$out" > "$identity_tmp"
  python3 - "$identity_tmp" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
identity = [r for r in rows if r["query"] == r["target"]]
if not identity:
    sys.exit("selftest: no identity rows in examples.tsv — nothing to check")
bad = [r for r in identity if r["rank"] != 1]
if bad:
    print("selftest FAILED: identity queries didn't rank 1 — plumbing is broken:", file=sys.stderr)
    for r in bad:
        print(f"  {r['query']!r} -> rank {r['rank']}", file=sys.stderr)
    sys.exit(1)
print(f"selftest: {len(identity)} identity queries all ranked 1", file=sys.stderr)
PY
  rm -f "$identity_tmp"

  echo "selftest: a query built from shell metacharacters survives the pipeline..." >&2
  local adversarial_tsv result_tmp
  adversarial_tsv="$(mktemp "$EVAL_DIR/selftest-adversarial.XXXXXX.tsv")"
  printf 'V\tComment `$(rm -rf /)`\tComment\n' > "$adversarial_tsv"
  out="$(run_set selftest "$adversarial_tsv" examples/schema.graphql)"
  rm -f "$adversarial_tsv"
  result_tmp="$(mktemp "$EVAL_DIR/selftest-result.XXXXXX")"
  printf '%s\n' "$out" > "$result_tmp"
  python3 - "$result_tmp" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
if len(rows) != 1:
    sys.exit(f"selftest FAILED: expected 1 answered row, got {len(rows)}")
r = rows[0]
if r["query"] != "Comment `$(rm -rf /)`":
    sys.exit(f"selftest FAILED: query text was mangled in transit: {r['query']!r}")
if r["rank"] != 1:
    sys.exit(f"selftest FAILED: expected rank 1, got {r['rank']}")
print("selftest: shell-metacharacter query round-tripped intact and ranked 1", file=sys.stderr)
PY
  rm -f "$result_tmp"

  echo "selftest: a query gqls rewrites still correlates to the right row..." >&2
  local rewrite_tsv
  rewrite_tsv="$(mktemp "$EVAL_DIR/selftest-rewrite.XXXXXX.tsv")"
  # gqls echoes the two-word form back as the rewritten qualified one
  # (`post author` -> `post.author`), which is exactly what broke the old
  # correlate-by-echoed-`query` approach: the lookup key it built never
  # matched the original tsv text, so this row scored a silent miss.
  printf 'Q\tpost author\tPost.author\n' > "$rewrite_tsv"
  out="$(run_set selftest "$rewrite_tsv" examples/schema.graphql)"
  rm -f "$rewrite_tsv"
  result_tmp="$(mktemp "$EVAL_DIR/selftest-result.XXXXXX")"
  printf '%s\n' "$out" > "$result_tmp"
  python3 - "$result_tmp" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
if len(rows) != 1:
    sys.exit(f"selftest FAILED: expected 1 answered row, got {len(rows)}")
r = rows[0]
if r["rank"] != 1:
    sys.exit(f"selftest FAILED: a query gqls rewrites didn't correlate back to its row (rank {r['rank']}) — correlation is likely keyed off the echoed query again")
print("selftest: rewritten query correlated correctly and ranked 1", file=sys.stderr)
PY
  rm -f "$result_tmp"

  echo "selftest: all checks passed" >&2
}

if [ "${1:-}" = "--selftest" ]; then
  selftest
  exit 0
fi

if [ ! -f "$GITHUB_SCHEMA" ]; then
  echo "fetching GitHub's schema (cached after this, at $GITHUB_SCHEMA)…" >&2
  if curl -fsSL -o "$GITHUB_SCHEMA.tmp" "$GITHUB_SCHEMA_URL"; then
    mv "$GITHUB_SCHEMA.tmp" "$GITHUB_SCHEMA"
  else
    rm -f "$GITHUB_SCHEMA.tmp"
    echo "no network and no cached schema - skipping script/eval/github.tsv" >&2
  fi
fi

if [ ! -f "$HOLDOUT_SCHEMA" ]; then
  echo "fetching AniList's schema (cached after this, at $HOLDOUT_SCHEMA)…" >&2
  # A standard introspection query, POSTed once; the response is the same
  # `{"data": {"__schema": ...}}` shape gqls accepts as a `.json` source
  # directly, so it's cached verbatim with no reshaping.
  HOLDOUT_QUERY='query IntrospectionQuery { __schema { queryType { name } mutationType { name } subscriptionType { name } types { ...FullType } directives { name description locations args { ...InputValue } } } } fragment FullType on __Type { kind name description fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated deprecationReason } inputFields { ...InputValue } interfaces { ...TypeRef } enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason } possibleTypes { ...TypeRef } } fragment InputValue on __InputValue { name description type { ...TypeRef } defaultValue } fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }'
  if curl -fsSL -X POST "$HOLDOUT_URL" -H 'Content-Type: application/json' -d "{\"query\": \"$HOLDOUT_QUERY\"}" -o "$HOLDOUT_SCHEMA.tmp"; then
    mv "$HOLDOUT_SCHEMA.tmp" "$HOLDOUT_SCHEMA"
  else
    rm -f "$HOLDOUT_SCHEMA.tmp"
    echo "no network and no cached schema - skipping script/eval/holdout.tsv" >&2
  fi
fi

if [ ! -f "$HASURA_SCHEMA" ]; then
  echo "fetching PokeAPI's schema (cached after this, at $HASURA_SCHEMA)…" >&2
  # Same standard introspection query as the holdout fetch above, POSTed to a
  # public Hasura endpoint instead - PokeAPI's is the README's own example of
  # the "every name shares a prefix" weak case, so it's the natural target
  # for the adversarial set.
  HASURA_QUERY='query IntrospectionQuery { __schema { queryType { name } mutationType { name } subscriptionType { name } types { ...FullType } directives { name description locations args { ...InputValue } } } } fragment FullType on __Type { kind name description fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated deprecationReason } inputFields { ...InputValue } interfaces { ...TypeRef } enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason } possibleTypes { ...TypeRef } } fragment InputValue on __InputValue { name description type { ...TypeRef } defaultValue } fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }'
  if curl -fsSL -X POST "$HASURA_URL" -H 'Content-Type: application/json' -d "{\"query\": \"$HASURA_QUERY\"}" -o "$HASURA_SCHEMA.tmp"; then
    mv "$HASURA_SCHEMA.tmp" "$HASURA_SCHEMA"
  else
    rm -f "$HASURA_SCHEMA.tmp"
    echo "no network and no cached schema - skipping script/eval/hasura.tsv" >&2
  fi
fi

SETS="examples script/eval/examples.tsv examples/schema.graphql"$'\n'
if [ -f "$GITHUB_SCHEMA" ]; then
  SETS+="github script/eval/github.tsv $GITHUB_SCHEMA"$'\n'
fi
if [ -f "$HOLDOUT_SCHEMA" ]; then
  SETS+="holdout script/eval/holdout.tsv $HOLDOUT_SCHEMA"$'\n'
fi
if [ -f "$HASURA_SCHEMA" ]; then
  SETS+="hasura script/eval/hasura.tsv $HASURA_SCHEMA"$'\n'
fi

RAW_FILE="$EVAL_DIR/last_run.jsonl"
: > "$RAW_FILE"
while IFS=' ' read -r name tsv schema; do
  [ -z "$name" ] && continue
  run_set "$name" "$tsv" "$schema" >> "$RAW_FILE"
done <<<"$SETS"

# The same identity check as --selftest, run every time against whatever just
# ran (not just the offline examples set): free, since run_set already
# produced this data, and it's the cheapest possible tripwire for the
# plumbing silently breaking on any set, not just the one --selftest covers
# offline.
python3 - "$RAW_FILE" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
bad = [r for r in rows if r["query"] == r["target"] and r["rank"] != 1]
if bad:
    print("eval: self-check failed - exact-name queries must rank 1 (plumbing is broken, not the ranking):", file=sys.stderr)
    for r in bad:
        print(f"  [{r['set']}/{r['category']}] {r['query']!r} -> rank {r['rank']}", file=sys.stderr)
    sys.exit(1)
PY

case "${1:-}" in
  --json) cat "$RAW_FILE" ;;
  --save)
    cp "$RAW_FILE" "$BASELINES/${2:?name required}.jsonl"
    echo "saved baseline '${2}'" >&2
    ;;
  --diff)
    python3 - "$BASELINES/${2:?name required}.jsonl" "$RAW_FILE" <<'PY'
import json, sys
from collections import defaultdict

def load(path):
    return [json.loads(l) for l in open(path) if l.strip()]

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

base_rows = load(sys.argv[1])
now_rows = load(sys.argv[2])
base = summarize(base_rows)
now = summarize(now_rows)

def fmt(v):
    return f"{v:.2f}"

print(f"{'set/category':<20}{'hit@1':>20}{'hit@5':>20}{'MRR':>20}")
for key in now:
    b, n = base.get(key), now[key]
    label = "/".join(key)
    if key[0] == "holdout":
        label += " (HELD-OUT)"
    elif key[0] == "hasura":
        label += " (ADVERSARIAL)"
    if b is None:
        cells = [f"{'—':>9}->{fmt(v):<9}" for v in n]
    else:
        cells = [f"{fmt(bv):>9}->{fmt(nv):<9}" for bv, nv in zip(b, n)]
    print(f"{label:<20}" + "".join(f"{c:>20}" for c in cells))

# Per-query movement: the aggregate table can hide a change that rescued nine
# queries and broke four by netting out to a small MRR delta, so list every
# query whose rank moved, one for one. Only queries present in both runs are
# comparable — a query added or removed between baselines isn't movement.
base_by_key = {(r["set"], r["query"], r["target"]): r for r in base_rows}
rescued, lost = [], []
for r in now_rows:
    key = (r["set"], r["query"], r["target"])
    b = base_by_key.get(key)
    if b is None or b["rank"] == r["rank"]:
        continue
    old, new = b["rank"], r["rank"]
    old_v = old if old is not None else float("inf")
    new_v = new if new is not None else float("inf")
    entry = (r["set"], r["category"], r["query"], r["target"], old, new)
    (rescued if new_v < old_v else lost).append(entry)

def fmt_rank(rank):
    return str(rank) if rank is not None else "miss"

def print_group(title, items):
    print(f"\n{title} (n={len(items)}):")
    if not items:
        print("  (none)")
        return
    for set_name, cat, query, target, old, new in sorted(items):
        print(f"  [{set_name}/{cat}] {query!r} -> {target}: {fmt_rank(old)} -> {fmt_rank(new)}")

if not rescued and not lost:
    print("\nmovement: none")
else:
    print_group("rescued", rescued)
    print_group("lost", lost)
PY
    ;;
  *)
    python3 - "$RAW_FILE" "$LIMIT" <<'PY'
import json, sys
from collections import defaultdict

rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
limit = sys.argv[2]
by_set = defaultdict(list)
for r in rows:
    by_set[r["set"]].append(r)

for set_name, set_rows in by_set.items():
    label = f"{set_name} (n={len(set_rows)})"
    if set_name == "holdout":
        label += "  — HELD-OUT: do not tune ranking against these numbers"
    elif set_name == "hasura":
        label += "  — ADVERSARIAL: a known-weak surface, fair game for tuning"
    print(f"\n=== {label} ===")
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
            rank = r["rank"] if r["rank"] is not None else f"not in top {limit}"
            print(f"  [{r['category']}] {r['query']!r} -> {r['target']}  rank={rank}")
PY
    ;;
esac
