#!/usr/bin/env bash
#
# The pre-push gate: formatting, lints, and tests. Stops at the first failure
# with a nonzero status.
#
#     script/check.sh
#
# Why a script rather than a chain of cargo commands: a shell pipeline's exit
# status is its LAST command's, so `cargo clippy … | tail -1` reports success
# even when clippy failed, and a `&& echo all-green` after it lies. `pipefail`
# below makes that impossible here; don't filter cargo through head/tail when
# you're gating on the result.
#
# If a build ever fails with an error that contradicts the source — an arity
# mismatch pointing at a signature that plainly has the right number of
# parameters — the target dir has a stale artifact. `cargo clean -p gqls-cli`
# clears it.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n=== %s\n' "$*"; }

# Drop this crate's artifacts before checking anything. Cargo's fingerprint for
# a crate can wedge "fresh" — observed here more than once — after which it
# reports success without recompiling changed sources, and the gate cheerfully
# validates code that isn't the code you wrote. Only gqls-cli is rebuilt (the
# expensive dependencies stay cached), which costs about five seconds and makes
# a green run mean what it says. `cargo clean -p` is also the manual fix if a
# build ever reports an error that contradicts the source.
step "clean (this crate only)"
cargo clean -p gqls-cli

step "fmt"
cargo fmt --check

step "clippy"
cargo clippy --all-targets -- -D warnings

step "tests"
cargo test

printf '\nall green\n'
