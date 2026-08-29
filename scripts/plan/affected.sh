#!/usr/bin/env bash
set -euo pipefail

plan_file="${CARGO_RAIL_PLAN_FILE:-}"
own_plan=false
if [ -z "$plan_file" ]; then
  plan_file="$(mktemp "${TMPDIR:-/tmp}/cargo-rail-plan-v8.XXXXXX")"
  own_plan=true
  scripts/plan/read.py create "$plan_file"
else
  scripts/plan/read.py validate "$plan_file"
fi
cleanup() {
  if [ "$own_plan" = true ]; then
    rm -f -- "$plan_file"
  fi
}
trap cleanup EXIT

export CARGO_RAIL_PLAN_FILE="$plan_file"
scripts/plan/verify.sh "$plan_file"
scripts/check/check.sh --affected
scripts/cargo/run.sh build
scripts/cargo/run.sh test

if [ "$(scripts/plan/read.py is-required "$plan_file" surface)" = true ]; then
  cargo rail surface --check --explain
else
  echo "Skipped Surface: not required by the plan."
fi

if [ "$(scripts/plan/read.py is-required "$plan_file" compiler-fact-driver)" = true ]; then
  scripts/check-compiler-fact-driver.sh
else
  echo "Skipped compiler fact driver: not required by the plan."
fi
