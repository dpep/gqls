#!/usr/bin/env bash
#
# A repeatable performance baseline.
#
#     script/bench.sh              # run the suite, print a table
#     script/bench.sh --json       # emit raw phase data
#     script/bench.sh --save NAME  # store a baseline
#     script/bench.sh --diff NAME  # compare against one
#
# Why a script: every measurement is otherwise ad-hoc and thrown away, so
# "did that get faster?" turns into re-deriving the setup from memory. This
# fixes the corpus and the query set, so a change's effect is a diff.
#
# The corpus is generated, not committed — deterministic (fixed seed) and
# rebuilt only when missing.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BENCH_DIR="${TMPDIR:-/tmp}/gqls-bench"
SCHEMA="$BENCH_DIR/big.graphql"
BASELINES="$BENCH_DIR/baselines"
RUNS="${RUNS:-5}"
GQLS="target/release/gqls"

# `flags|query` — the query stays quoted so a multi-word one isn't split into
# a query plus a source, while the flags are meant to word-split.
#
# Wildcards and qualified forms take different paths through search, and the
# semantic combine is the slowest thing gqls does — cover each.
QUERIES=(
  "--fuzzy|user"
  "--fuzzy|usre"
  "--fuzzy|AdminUserAdmin.email"
  "--fuzzy|AdminUserAdmin."
  "--fuzzy|*.employees"
  "|employee data"
)

mkdir -p "$BENCH_DIR" "$BASELINES"

if [ ! -f "$SCHEMA" ]; then
  echo "generating corpus…" >&2
  python3 - "$SCHEMA" <<'PY'
import random, sys
random.seed(42)
nouns = ["User","Company","Invoice","Payment","Employee","Report","Account",
         "Ledger","Payroll","Tax","Benefit","Contractor","Team","Role",
         "Permission","Audit","Session","Token","Webhook","Event"]
mods = ["Admin","Public","Internal","Legacy","Draft","Archived","Pending",
        "Active","Batch","Summary","Detail","Profile","Settings","Stats",
        "Onboarding","Offboarding","Sync","Export","Import","Preview"]
fields = ["id","name","email","status","createdAt","updatedAt","amount","total",
          "count","items","owner","members","employees","notes","tags","type"]
types = [f"{a}{b}{c}" for a in mods for b in nouns for c in mods[:8]][:3000]
out = ["type Query {"]
out += [f"  get{t}(id: ID!): {t}" for t in types[:500]]
out.append("}")
for t in types:
    out.append(f"type {t} {{")
    out += [f"  {f}: String" for f in random.sample(fields, 15)]
    out.append("}")
open(sys.argv[1], "w").write("\n".join(out) + "\n")
PY
fi

[ -x "$GQLS" ] || { echo "build first: cargo build --release" >&2; exit 1; }

export GQLS_NO_AUTOWARM=1

# Warm the caches once so the numbers measure steady state, not first-run
# parsing — the cold path is a separate question from "is a query fast".
$GQLS user "$SCHEMA" --fuzzy -q -l 1 >/dev/null 2>&1 || true

results=$(
  for entry in "${QUERIES[@]}"; do
    flags="${entry%%|*}"
    q="${entry#*|}"
    for _ in $(seq 1 "$RUNS"); do
      # `$flags` is meant to split into words; `$q` must not, or a two-word
      # query becomes a query plus a source. stderr carries status chatter as
      # well as the profile, so keep the one JSON object and drop the prose.
      # shellcheck disable=SC2086
      $GQLS $flags "$q" "$SCHEMA" -j -l 5 --profile -q 2>&1 >/dev/null |
        grep '"total_ms"' |
        python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(json.dumps({'query': '''$q''', **d}))"
    done
  done
)

case "${1:-}" in
  --json) echo "$results" ;;
  --save)
    echo "$results" > "$BASELINES/${2:?name required}.jsonl"
    echo "saved baseline '${2}'" >&2
    ;;
  --diff)
    python3 - "$BASELINES/${2:?name required}.jsonl" <<PY
import json, sys, statistics as st
def load(lines):
    runs = {}
    for l in lines:
        if l.strip():
            d = json.loads(l)
            runs.setdefault(d["query"], []).append(d["total_ms"])
    return {q: st.median(v) for q, v in runs.items()}
base = load(open(sys.argv[1]))
now = load("""$results""".splitlines())
print(f"{'query':<28}{'baseline':>10}{'now':>10}{'change':>10}")
for q in now:
    b, n = base.get(q), now[q]
    if b is None:
        print(f"{q:<28}{'—':>10}{n:>9.1f}ms{'new':>10}")
    else:
        pct = (n - b) / b * 100
        print(f"{q:<28}{b:>9.1f}ms{n:>9.1f}ms{pct:>+9.1f}%")
PY
    ;;
  *)
    python3 - <<PY
import json, statistics as st
from collections import defaultdict
runs, phases = defaultdict(list), defaultdict(lambda: defaultdict(list))
for l in """$results""".splitlines():
    if not l.strip():
        continue
    d = json.loads(l)
    runs[d["query"]].append(d["total_ms"])
    for p in d["phases"]:
        phases[d["query"]][p["name"]].append(p["ms"])
print(f"{'query':<28}{'median':>10}{'min':>9}{'max':>9}   slowest phase")
for q, v in runs.items():
    slow = max(((n, st.median(t)) for n, t in phases[q].items()), key=lambda x: x[1], default=("—", 0))
    print(f"{q:<28}{st.median(v):>9.1f}ms{min(v):>8.1f}ms{max(v):>8.1f}ms   {slow[0]} ({slow[1]:.1f}ms)")
PY
    ;;
esac
