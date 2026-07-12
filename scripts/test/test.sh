#!/usr/bin/env bash
set -euo pipefail

# Smart Test Runner
#
# Uses cargo-rail for intelligent change detection.
# Uses a single planner/executor invocation in smart mode.
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
if [ "$MODE" = "commit" ]; then
  # CI mode: force commit profile and nextest JUnit output path.
  "${RAIL_CMD[@]}" run "${PLAN_ARGS[@]}" --surface test --explain \
    --test-runner nextest \
    --nextest-arg=-P \
    --nextest-arg="$NEXTEST_PROFILE" \
    --nextest-arg=--config-file \
    --nextest-arg=.config/nextest.toml
else
  "${RAIL_CMD[@]}" run "${PLAN_ARGS[@]}" --surface test --explain
fi
