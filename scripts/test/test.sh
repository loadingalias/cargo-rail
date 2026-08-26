#!/usr/bin/env bash
set -euo pipefail

# Planner-scoped tests
#
# Cargo-Rail owns package selection. cargo-nextest owns execution.
#
# Usage:
#   ./test.sh              # Smart: test affected crates only
#   ./test.sh <crate>      # Direct: test specific crate
#   ./test.sh --all        # Full: test entire workspace
#
# Environment:
#   CARGO_RAIL_TEST_MODE   # "local" (default) or "commit" (CI)
#   RAIL_SINCE             # Git ref for CI comparison

ARG="${1:-}"
MODE="${CARGO_RAIL_TEST_MODE:-local}"

# Select nextest profile based on mode
if [ "$MODE" = "commit" ]; then
  NEXTEST_PROFILE="commit"
else
  NEXTEST_PROFILE="default"
fi

echo "Running Tests"
echo "Mode: $MODE | Nextest profile: $NEXTEST_PROFILE"
echo ""

# Full workspace test (no change detection)
if [ "$ARG" = "--all" ]; then
  echo "Full workspace test (--all)"
  echo ""
  cargo nextest run --workspace -P "$NEXTEST_PROFILE" --all-features --locked --config-file .config/nextest.toml
  cargo test --doc -p cargo-rail --all-features --locked
  exit 0
fi

# Specific crate: Direct test
if [ -n "$ARG" ]; then
  echo "Testing specific crate: $ARG"
  echo ""
  cargo nextest run -p "$ARG" -P "$NEXTEST_PROFILE" --all-features --locked --config-file .config/nextest.toml
  exit 0
fi

echo "Testing affected crates..."
PLAN_READER="scripts/plan/read.py"
PLAN_FILE="${CARGO_RAIL_PLAN_FILE:-}"
OWN_PLAN=false
if [ -z "$PLAN_FILE" ]; then
  PLAN_FILE="$(mktemp "${TMPDIR:-/tmp}/cargo-rail-plan-v8.XXXXXX")"
  OWN_PLAN=true
  "$PLAN_READER" create "$PLAN_FILE"
else
  "$PLAN_READER" validate "$PLAN_FILE"
fi
"$PLAN_READER" verify-checkout "$PLAN_FILE"
cleanup() {
  if [ "$OWN_PLAN" = true ]; then
    rm -f -- "$PLAN_FILE"
  fi
}
trap cleanup EXIT

if [ "$("$PLAN_READER" is-required "$PLAN_FILE" cargo.test)" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r -d '' argument; do
    CARGO_ARGS+=("$argument")
  done < <("$PLAN_READER" cargo-args "$PLAN_FILE" cargo.test)
  while IFS= read -r -d '' argument; do
    CARGO_ARGS+=("$argument")
  done < <("$PLAN_READER" target-args "$PLAN_FILE" cargo.test)

  cargo nextest run "${CARGO_ARGS[@]}" -P "$NEXTEST_PROFILE" --all-features --locked --config-file .config/nextest.toml
else
  echo "No affected nextest targets."
fi

if [ "$("$PLAN_READER" is-required "$PLAN_FILE" cargo.doctest)" = "true" ]; then
  DOCTEST_ARGS=()
  while IFS= read -r -d '' argument; do
    DOCTEST_ARGS+=("$argument")
  done < <("$PLAN_READER" cargo-args "$PLAN_FILE" cargo.doctest)
  cargo test --doc "${DOCTEST_ARGS[@]}" --all-features --locked
else
  echo "No affected doctest targets."
fi
