#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <plan-v8.json>" >&2
  exit 2
fi

plan_file="$1"
bundle_manifest="$(dirname "$plan_file")/plan-bundle-v1.json"
bundle_reader="$(dirname "$plan_file")/plan-read.py"

if [ -f "$bundle_manifest" ] || [ -f "$bundle_reader" ]; then
  if [ ! -f "$bundle_manifest" ] || [ ! -f "$bundle_reader" ]; then
    echo "plan bundle is incomplete" >&2
    exit 2
  fi
  exec python3 "$bundle_reader" verify-bundle "$bundle_manifest"
fi

# Legacy unbundled plans retain binary verification. Portable object-bound
# artifacts take the manifest path above and never bootstrap a host binary.
if [ -z "${CARGO_RAIL_BIN:-}" ] && [ -z "${RAIL_BOOTSTRAP_TARGET_DIR+x}" ] && \
  [ "${GITHUB_ACTIONS:-}" = true ]; then
  if [ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] && \
    [ -f target/debug/cargo-rail ]; then
    chmod u+x target/debug/cargo-rail
    export CARGO_RAIL_BIN=target/debug/cargo-rail
    printf '%s\n' "$PWD/target/debug" >> "${GITHUB_PATH:?GITHUB_PATH is required in GitHub Actions}"
  else
    export RAIL_BOOTSTRAP_TARGET_DIR=target/cargo-rail-bootstrap
  fi
fi

exec scripts/plan/read.py verify-checkout "$plan_file"
