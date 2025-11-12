#!/usr/bin/env bash
set -euo pipefail

CRATE="${1:-}"

echo "Running Unit, Integration, and Property Tests via Nextest..."
export CARGO_RAIL_TEST_MODE=${CARGO_RAIL_TEST_MODE:-local}
echo "Test mode: $CARGO_RAIL_TEST_MODE"

# Select nextest profile based on test mode
case "$CARGO_RAIL_TEST_MODE" in
  commit)
    PROFILE="commit"
    ;;
  local | *)
    PROFILE="default"
    ;;
esac

echo "Using nextest profile: $PROFILE"

if [ -n "$CRATE" ]; then
  # User specified a crate explicitly
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "🎯 Running tests for specific crate: $CRATE"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  cargo nextest run -p "$CRATE" -P "$PROFILE" --all-features --config-file .config/nextest.toml
else
  # Full workspace test
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "🔄 Running tests for entire workspace"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  cargo nextest run --workspace -P "$PROFILE" --all-features --config-file .config/nextest.toml
fi
