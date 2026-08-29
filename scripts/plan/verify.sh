#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <plan-v8.json>" >&2
  exit 2
fi

plan_file="$1"

# The Commit planner artifact carries the exact current-source Linux planner.
# Other CI hosts rebuild that source in an isolated target because a foreign
# executable cannot authenticate the checkout. Local use remains on the
# installed release selected by read.py.
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
