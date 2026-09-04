#!/usr/bin/env bash
set -euo pipefail

operation="${1:-}"
argument="${2:-}"
if [ "$#" -gt 2 ]; then
  echo "usage: $0 <build|test> [crate|--all]" >&2
  exit 2
fi

case "$operation" in
  build)
    if [ -n "$argument" ]; then
      echo "usage: $0 build" >&2
      exit 2
    fi
    ;;
  test) ;;
  *)
    echo "usage: $0 <build|test> [crate|--all]" >&2
    exit 2
    ;;
esac

if [ "$operation" = test ] && [ "$argument" = --all ]; then
  cargo nextest run --workspace -P default --all-features --locked \
    --config-file .config/nextest.toml
  cargo test --doc -p cargo-rail --all-features --locked
  exit 0
fi

if [ "$operation" = test ] && [ -n "$argument" ]; then
  cargo nextest run -p "$argument" -P default --all-features --locked \
    --config-file .config/nextest.toml
  exit 0
fi

plan_reader=scripts/plan/read.py
plan_file="${CARGO_RAIL_PLAN_FILE:-}"
own_plan=false
if [ -z "$plan_file" ]; then
  plan_file="$(mktemp "${TMPDIR:-/tmp}/cargo-rail-plan-v8.XXXXXX")"
  own_plan=true
  "$plan_reader" create "$plan_file"
else
  "$plan_reader" validate "$plan_file"
fi
cleanup() {
  if [ "$own_plan" = true ]; then
    rm -f -- "$plan_file"
  fi
}
trap cleanup EXIT

run_cargo_work() {
  local work="$1"
  shift
  if [ "$("$plan_reader" is-required "$plan_file" "$work")" != true ]; then
    echo "Skipped $work: not required by the plan."
    return
  fi

  local cargo_args=()
  while IFS= read -r -d '' value; do
    cargo_args+=("$value")
  done < <("$plan_reader" cargo-args "$plan_file" "$work")
  if [ "$work" = cargo.test ]; then
    while IFS= read -r -d '' value; do
      cargo_args+=("$value")
    done < <("$plan_reader" target-args "$plan_file" "$work")
  fi

  scripts/plan/verify.sh "$plan_file"
  cargo "$@" "${cargo_args[@]}"
}

if [ "$operation" = build ]; then
  run_cargo_work cargo.build build --all-targets --all-features --locked
  exit 0
fi

run_cargo_work cargo.test nextest run -P default --all-features --locked \
  --config-file .config/nextest.toml
run_cargo_work cargo.doctest test --doc --all-features --locked
