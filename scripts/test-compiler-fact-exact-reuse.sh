#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root

: "${CARGO_RAIL_TEST_FACT_DRIVER:?compiler fact driver authority is required}"

cargo build --locked -p cargo-rail --all-features --bin cargo-rail
observation_wrapper="$repository_root/target/debug/cargo-rail"
if [[ "${OS:-}" == Windows_NT ]]; then
  observation_wrapper="$observation_wrapper.exe"
fi
CARGO_RAIL_TEST_OBSERVATION_WRAPPER="$observation_wrapper" \
  cargo nextest run --locked -p cargo-rail --all-features --no-capture \
    --run-ignored ignored-only exact_compiler_fact_cas_reuse_eliminates_independent_acquisitions
