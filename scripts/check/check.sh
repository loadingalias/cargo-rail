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
AFFECTED=false

for arg in "$@"; do
  case "$arg" in
    --fix) FIX_MODE=true ;;
    --affected) AFFECTED=true ;;
    *) echo "Usage: $0 [--fix] [--affected]" >&2; exit 2 ;;
  esac
done

if [ "$FIX_MODE" = true ] && [ "$AFFECTED" = true ]; then
  echo "error: --fix cannot be narrowed by --affected" >&2
  exit 2
fi

PLAN_READER="scripts/plan/read.py"
PLAN_FILE="${CARGO_RAIL_PLAN_FILE:-}"
OWN_PLAN=false
if [ "$AFFECTED" = true ]; then
  if [ -z "$PLAN_FILE" ]; then
    PLAN_FILE="$(mktemp "${TMPDIR:-/tmp}/cargo-rail-plan-v8.XXXXXX")"
    OWN_PLAN=true
    "$PLAN_READER" create "$PLAN_FILE"
  else
    "$PLAN_READER" validate "$PLAN_FILE"
  fi
fi
cleanup() {
  if [ "$OWN_PLAN" = true ]; then
    rm -f -- "$PLAN_FILE"
  fi
}
trap cleanup EXIT

required() {
  if [ "$AFFECTED" = false ]; then
    return 0
  fi
  [ "$("$PLAN_READER" is-required "$PLAN_FILE" "$1")" = "true" ]
}

echo "Running quality checks"
echo ""

# Fast checks first (catch issues early)
echo "Formatting..."
if required cargo.fmt; then
  if [ "$FIX_MODE" = true ]; then
    cargo fmt --all
  else
    cargo fmt --all -- --check
  fi
else
  echo "Skipped formatting: cargo.fmt is not required."
fi

echo "Locked Cargo command surfaces..."
if required cargo-command-policy; then
  scripts/ci/check-locked-cargo.sh
  scripts/ci/check-locked-cargo-test.sh
else
  echo "Skipped locked Cargo command policy."
fi

echo "Workflow action pins..."
if required action-pins; then
  scripts/ci/pin-actions.sh --verify-only
else
  echo "Skipped workflow action-pin verification."
fi

echo "Installers..."
if required installers; then
  scripts/ci/check-installers.sh
else
  echo "Skipped installer checks."
fi

echo "Dependency and security policy..."
if required dependency-policy; then
  cargo rail unify --check --explain
  cargo deny --locked check -D warnings all
else
  echo "Skipped dependency policy."
fi

echo "Reviewed release intent..."
if required cargo.package; then
  if [ "${CARGO_RAIL_OPERATION:-}" = release ] && [ "${CARGO_RAIL_RELEASE_PUSH:-}" = 1 ]; then
    echo "Release transaction consumed reviewed change files."
  else
    cargo rail change check --merge-base --required
  fi
else
  echo "Skipped change-file coverage."
fi

# Clippy performs Cargo's check pass, so do not run a separate `cargo check` first.
echo "Linting..."
if required cargo.clippy; then
  CLIPPY_ARGS=()
  if [ "$AFFECTED" = true ]; then
    while IFS= read -r -d '' argument; do
      CLIPPY_ARGS+=("$argument")
    done < <("$PLAN_READER" cargo-args "$PLAN_FILE" cargo.clippy)
  else
    CLIPPY_ARGS=(--workspace)
  fi
  if [ "$AFFECTED" = true ]; then
    scripts/plan/verify.sh "$PLAN_FILE"
  fi
  if [ "$FIX_MODE" = true ]; then
    cargo clippy "${CLIPPY_ARGS[@]}" --all-targets --all-features --locked --fix
  else
    cargo clippy "${CLIPPY_ARGS[@]}" --all-targets --all-features --locked
  fi
else
  echo "Skipped Clippy."
fi

# Generated docs must match the CLI and executable support authorities.
echo "Generated documentation..."
if required docs.generated; then
  scripts/docs/generate.sh --check
else
  echo "Skipped generated documentation."
fi

# Docs always full workspace (cross-crate links require it)
echo "Documentation..."
if required cargo.doc; then
  DOC_ARGS=()
  if [ "$AFFECTED" = true ]; then
    while IFS= read -r -d '' argument; do
      DOC_ARGS+=("$argument")
    done < <("$PLAN_READER" cargo-args "$PLAN_FILE" cargo.doc)
  else
    DOC_ARGS=(--workspace)
  fi
  if [ "$AFFECTED" = true ]; then
    scripts/plan/verify.sh "$PLAN_FILE"
  fi
  RUSTDOCFLAGS="-D warnings" cargo doc "${DOC_ARGS[@]}" --no-deps --all-features --locked
else
  echo "Skipped Cargo documentation."
fi

echo ""
echo "All checks passed."
