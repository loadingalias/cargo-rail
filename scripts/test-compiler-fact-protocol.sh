#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root
readonly companion_manifest="$repository_root/tools/compiler-fact-driver/Cargo.toml"

env RUSTC_BOOTSTRAP=cargo_rail_fact_driver \
  cargo build --locked --manifest-path "$companion_manifest"

driver="$repository_root/tools/compiler-fact-driver/target/debug/cargo-rail-fact-driver"
if [[ "${OS:-}" == "Windows_NT" ]]; then
  driver="$driver.exe"
fi

if [[ "${OS:-}" != "Windows_NT" ]]; then
  CARGO_RAIL_TEST_FACT_DRIVER="$driver" \
    cargo nextest run --locked -p cargo-rail --all-features \
      --run-ignored ignored-only manufactured_driver_fragment_passes_the_stable_admission_boundary
fi

CARGO_RAIL_TEST_FACT_DRIVER="$driver" \
  cargo nextest run --locked -p cargo-rail --all-features \
    --run-ignored ignored-only stable_wrapper_authorizes_the_matched_driver_per_compilation_unit

fact_cache="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-fact-cache.XXXXXX")"
trap 'rm -rf "$fact_cache"' EXIT
CARGO_RAIL_CACHE_DIR="$fact_cache" \
  CARGO_RAIL_CACHE_MAX_BYTES=1073741824 \
  "$repository_root/scripts/with-compiler-fact-driver.sh" \
  "$repository_root/scripts/test-compiler-fact-exact-reuse.sh"

if [[ "${OS:-}" != "Windows_NT" ]]; then
  CARGO_RAIL_TEST_FACT_DRIVER="$driver" \
    cargo nextest run --locked -p cargo-rail --all-features \
      --run-ignored ignored-only stable_rustdoc_proxy_authorizes_generated_doctest_units
fi
