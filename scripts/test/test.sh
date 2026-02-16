#!/usr/bin/env bash
set -euo pipefail

# Smart Test Runner
#
# Uses cargo-rail for intelligent change detection.
# Always shows the plan before executing.
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

# Determine comparison ref
if [ -n "${RAIL_SINCE:-}" ]; then
  PLAN_ARGS="--since $RAIL_SINCE"
else
  PLAN_ARGS="--merge-base"
fi

echo "Running Tests"
echo "Mode: $MODE | Nextest profile: $NEXTEST_PROFILE"
echo ""

# Full workspace test (no change detection)
if [ "$ARG" = "--all" ]; then
  echo "Full workspace test (--all)"
  echo ""
  cargo nextest run --workspace -P "$NEXTEST_PROFILE" --all-features --config-file .config/nextest.toml
  exit 0
fi

# Specific crate: Direct test
if [ -n "$ARG" ]; then
  echo "Testing specific crate: $ARG"
  echo ""
  cargo nextest run -p "$ARG" -P "$NEXTEST_PROFILE" --all-features --config-file .config/nextest.toml
  exit 0
fi

# Smart mode: Use cargo-rail for change detection
echo "Change Detection Plan:"
echo ""
cargo rail plan $PLAN_ARGS --explain
echo ""

echo "Testing affected crates..."
if [ "$MODE" = "commit" ]; then
  # CI mode: force commit profile and nextest JUnit output path.
  cargo rail run $PLAN_ARGS --surface test -- -P "$NEXTEST_PROFILE" --config-file .config/nextest.toml
else
  cargo rail run $PLAN_ARGS --surface test
fi
