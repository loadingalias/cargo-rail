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

# Run the control-plane cargo-rail binary from an isolated target dir.
# On Windows, `cargo nextest` cannot rebuild `target/debug/cargo-rail.exe`
# while that exact binary is still running via `cargo run`.
RAIL_BOOTSTRAP_TARGET_DIR="${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}"
RAIL_CMD=(cargo run --quiet --locked --target-dir "$RAIL_BOOTSTRAP_TARGET_DIR" -- rail)

ARG="${1:-}"
MODE="${CARGO_RAIL_TEST_MODE:-local}"

# Select nextest profile based on mode
if [ "$MODE" = "commit" ]; then
  NEXTEST_PROFILE="commit"
else
  NEXTEST_PROFILE="default"
fi

# Determine comparison ref
if [ -n "${RAIL_SINCE:-}" ]; then
  PLAN_ARGS=(--since "$RAIL_SINCE")
else
  PLAN_ARGS=(--merge-base)
fi

echo "Running Tests"
echo "Mode: $MODE | Nextest profile: $NEXTEST_PROFILE"
echo ""

# Full workspace test (no change detection)
if [ "$ARG" = "--all" ]; then
  echo "Full workspace test (--all)"
  echo ""
  cargo nextest run --workspace -P "$NEXTEST_PROFILE" --all-features --locked --config-file .config/nextest.toml
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
if ! command -v jq >/dev/null 2>&1; then
  echo "error: smart tests require jq to read 'cargo rail plan -f json'" >&2
  exit 2
fi

PLAN_JSON="$("${RAIL_CMD[@]}" plan "${PLAN_ARGS[@]}" -f json)"
if ! jq -e '
  .plan_contract_version == 7 and
  .scope.scope_contract_version == 4 and
  (.surfaces.test.enabled | type == "boolean") and
  (.surfaces.test.scope.cargo_args | type == "array") and
  all(.surfaces.test.scope.cargo_args[]; type == "string")
' <<<"$PLAN_JSON" >/dev/null; then
  echo "error: cargo-rail returned an unsupported test scope contract" >&2
  exit 2
fi

if [ "$(jq -r '.surfaces.test.enabled' <<<"$PLAN_JSON")" != "true" ]; then
  echo "No affected test targets."
  exit 0
fi

CARGO_ARGS=()
while IFS= read -r argument; do
  CARGO_ARGS+=("$argument")
done < <(jq -r '.surfaces.test.scope.cargo_args[]' <<<"$PLAN_JSON")

cargo nextest run "${CARGO_ARGS[@]}" -P "$NEXTEST_PROFILE" --all-features --locked --config-file .config/nextest.toml
