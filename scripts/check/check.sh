#!/usr/bin/env bash
set -euo pipefail

# Quality Check Runner
#
# Uses cargo-rail for intelligent change detection. Checks are read-only;
# formatting and Clippy fixes require the explicit --fix flag.
#
# Usage:
#   ./check.sh              # Smart, read-only check
#   ./check.sh --all        # Full workspace, read-only check
#   ./check.sh --fix        # Format and fix affected crates, then check
#
# Environment:
#   RAIL_SINCE              # Git ref for comparison

# Run the control-plane cargo-rail binary from an isolated target dir.
# This avoids Windows file-lock conflicts when cargo-rail later asks Cargo
# to build or test the workspace's own `target` tree.
RAIL_BOOTSTRAP_TARGET_DIR="${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}"
RAIL_CMD=(cargo run --quiet --locked --target-dir "$RAIL_BOOTSTRAP_TARGET_DIR" -- rail)

# Parse arguments
FIX_MODE=false
FULL_WORKSPACE=false

for arg in "$@"; do
  case "$arg" in
    --fix) FIX_MODE=true ;;
    --all) FULL_WORKSPACE=true ;;
    *) echo "Usage: $0 [--all] [--fix]" >&2; exit 2 ;;
  esac
done

# Determine comparison ref
if [ -n "${RAIL_SINCE:-}" ]; then
  PLAN_ARGS=(--since "$RAIL_SINCE")
else
  PLAN_ARGS=(--merge-base)
fi

echo "Running quality checks"
echo ""

# Fast checks first (catch issues early)
echo "Formatting..."
if [ "$FIX_MODE" = true ]; then
  cargo fmt --all
else
  cargo fmt --all -- --check
fi

echo "Security audit..."
cargo deny check all
cargo audit

# Build surface checks
if [ "$FULL_WORKSPACE" = true ]; then
  echo "Full workspace checks..."
  cargo check --workspace --all-targets --all-features --locked
  if [ "$FIX_MODE" = true ]; then
    cargo clippy --workspace --all-targets --all-features --locked --fix --allow-dirty -- -D warnings
  else
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  fi
else
  echo "Smart checks (affected crates)..."
  "${RAIL_CMD[@]}" run "${PLAN_ARGS[@]}" --action build --explain
  if [ "$FIX_MODE" = true ]; then
    cargo clippy --workspace --all-targets --all-features --locked --fix --allow-dirty -- -D warnings
  else
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  fi
fi

# Docs always full workspace (cross-crate links require it)
echo "Documentation..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

echo ""
echo "All checks passed."
