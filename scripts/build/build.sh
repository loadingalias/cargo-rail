#!/usr/bin/env bash
set -euo pipefail

# Build the planner-selected Cargo packages directly. Cargo-Rail owns scope;
# Cargo owns execution and exit behavior.

if ! command -v jq >/dev/null 2>&1; then
  echo "error: smart builds require jq to read 'cargo rail plan -f json'" >&2
  exit 2
fi

RAIL_BOOTSTRAP_TARGET_DIR="${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}"
RAIL_CMD=(cargo run --quiet --locked --target-dir "$RAIL_BOOTSTRAP_TARGET_DIR" -- rail)

if [ -n "${RAIL_SINCE:-}" ]; then
  PLAN_ARGS=(--since "$RAIL_SINCE")
else
  PLAN_ARGS=(--merge-base)
fi

PLAN_JSON="$("${RAIL_CMD[@]}" plan "${PLAN_ARGS[@]}" -f json)"
if ! jq -e '
  .plan_contract_version == 7 and
  .scope.scope_contract_version == 4 and
  (.surfaces.build.enabled | type == "boolean") and
  (.surfaces.build.scope.cargo_args | type == "array") and
  all(.surfaces.build.scope.cargo_args[]; type == "string")
' <<<"$PLAN_JSON" >/dev/null; then
  echo "error: cargo-rail returned an unsupported build scope contract" >&2
  exit 2
fi

if [ "$(jq -r '.surfaces.build.enabled' <<<"$PLAN_JSON")" != "true" ]; then
  echo "No affected build targets."
  exit 0
fi

CARGO_ARGS=()
while IFS= read -r argument; do
  CARGO_ARGS+=("$argument")
done < <(jq -r '.surfaces.build.scope.cargo_args[]' <<<"$PLAN_JSON")

echo "Building affected packages..."
cargo build "${CARGO_ARGS[@]}" --all-targets --all-features --locked
