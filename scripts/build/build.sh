#!/usr/bin/env bash
set -euo pipefail

# Build the planner-selected Cargo packages directly. Cargo-Rail owns scope;
# Cargo owns execution and exit behavior.

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
cleanup() {
  if [ "$OWN_PLAN" = true ]; then
    rm -f -- "$PLAN_FILE"
  fi
}
trap cleanup EXIT

if [ "$("$PLAN_READER" is-required "$PLAN_FILE" cargo.build)" != "true" ]; then
  echo "No affected build targets."
  exit 0
fi

CARGO_ARGS=()
while IFS= read -r -d '' argument; do
  CARGO_ARGS+=("$argument")
done < <("$PLAN_READER" cargo-args "$PLAN_FILE" cargo.build)

"$PLAN_READER" verify-checkout "$PLAN_FILE"
echo "Building affected packages..."
cargo build "${CARGO_ARGS[@]}" --all-targets --all-features --locked
