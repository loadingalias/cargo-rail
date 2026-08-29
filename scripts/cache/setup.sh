#!/usr/bin/env bash
set -euo pipefail

arguments=("$@")
if [ -n "${CARGO_RAIL_CACHE_REMOTE:-}" ]; then
  mode="${CARGO_RAIL_CACHE_MODE:-read}"
  if [ "$mode" != read ] && [ "$mode" != read-write ]; then
    echo "CARGO_RAIL_CACHE_MODE must be read or read-write" >&2
    exit 2
  fi
  arguments+=(
    --remote "$CARGO_RAIL_CACHE_REMOTE"
    --remote-mode "$mode"
    --root-portability remap
  )
else
  arguments+=(--local-only)
fi

exec cargo rail cache setup "${arguments[@]}"
