#!/usr/bin/env bash
# Pre-push hook: runs quality checks before allowing push
# Symlink this to .git/hooks/pre-push or run: ln -sf ../../scripts/ci/pre-push.sh .git/hooks/pre-push

set -e

echo "Running pre-push checks..."
just check

echo "All checks passed."
