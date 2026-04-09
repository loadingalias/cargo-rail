#!/usr/bin/env bash
set -euo pipefail

# Quality Check Runner
#
# Uses cargo-rail for intelligent change detection.
# Shows the plan before executing.
#
# Usage:
#   ./check.sh              # Smart check with auto-fix
#   ./check.sh --all        # Full workspace with auto-fix
#   ./check.sh --ci         # Smart check, read-only (CI mode)
#   ./check.sh --ci --all   # Full workspace, read-only
#
# Environment:
#   CARGO_RAIL_TEST_MODE    # "commit" forces CI mode
#   RAIL_SINCE              # Git ref for comparison

# Run the control-plane cargo-rail binary from an isolated target dir.
# This avoids Windows file-lock conflicts when cargo-rail later asks Cargo
# to build or test the workspace's own `target` tree.
RAIL_BOOTSTRAP_TARGET_DIR="${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}"
RAIL_CMD=(cargo run --quiet --target-dir "$RAIL_BOOTSTRAP_TARGET_DIR" -- rail)

# Parse arguments
CI_MODE=false
FULL_WORKSPACE=false

for arg in "$@"; do
  case "$arg" in
    --ci) CI_MODE=true ;;
    --all) FULL_WORKSPACE=true ;;
  esac
done

# Environment can also trigger CI mode
if [ "${CARGO_RAIL_TEST_MODE:-}" = "commit" ]; then
  CI_MODE=true
fi

# Set mode-specific options
if [ "$CI_MODE" = true ]; then
  FMT_ARGS="--check"
  CLIPPY_FIX=""
  MODE_LABEL="CI"
else
  FMT_ARGS=""
  CLIPPY_FIX="--fix --allow-dirty"
  MODE_LABEL="local"
fi

# Determine comparison ref
if [ -n "${RAIL_SINCE:-}" ]; then
  PLAN_ARGS="--since $RAIL_SINCE"
else
  PLAN_ARGS="--merge-base"
fi

echo "Running Quality Checks ($MODE_LABEL)"
echo ""

# Show plan first (skip for --all since we're running everything)
if [ "$FULL_WORKSPACE" = false ]; then
  echo "Change Detection Plan:"
  echo ""
  "${RAIL_CMD[@]}" plan $PLAN_ARGS --explain
  echo ""
fi

# Fast checks first (catch issues early)
echo "Formatting..."
cargo fmt --all -- $FMT_ARGS

echo "Security audit..."
cargo deny check all
cargo audit

# Build surface checks
if [ "$FULL_WORKSPACE" = true ]; then
  echo "Full workspace checks..."
  cargo check --workspace --all-targets --all-features
  cargo clippy --workspace --all-targets --all-features $CLIPPY_FIX -- -D warnings
else
  echo "Smart checks (affected crates)..."
  "${RAIL_CMD[@]}" run $PLAN_ARGS --surface build
fi

# Docs always full workspace (cross-crate links require it)
echo "Documentation..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

echo ""
echo "All checks passed."
