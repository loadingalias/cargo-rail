#!/usr/bin/env bash
set -euo pipefail

# Quality Check Runner
#
# Checks are workspace-wide and read-only; formatting and Clippy fixes require
# the explicit --fix flag.
#
# Usage:
#   ./check.sh              # Full workspace, read-only check
#   ./check.sh --fix        # Format and fix the workspace, then check

# Parse arguments
FIX_MODE=false

for arg in "$@"; do
  case "$arg" in
    --fix) FIX_MODE=true ;;
    *) echo "Usage: $0 [--fix]" >&2; exit 2 ;;
  esac
done

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

# Clippy performs Cargo's check pass, so do not run a separate `cargo check` first.
echo "Linting..."
if [ "$FIX_MODE" = true ]; then
  cargo clippy --workspace --all-targets --all-features --locked --fix --allow-dirty -- -D warnings
else
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
fi

# Generated docs must match the CLI and native-cache production gates.
echo "Generated documentation..."
scripts/docs/generate.sh --check

# Docs always full workspace (cross-crate links require it)
echo "Documentation..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

echo ""
echo "All checks passed."
