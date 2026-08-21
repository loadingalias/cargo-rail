#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root
readonly manifest="$repository_root/tools/compiler-fact-driver/Cargo.toml"

cargo fmt --manifest-path "$manifest" -- --check --config-path "$repository_root/rustfmt.toml"
env RUSTC_BOOTSTRAP=cargo_rail_fact_driver \
  cargo clippy --locked --manifest-path "$manifest" --all-targets
"$repository_root/scripts/test-compiler-fact-driver.sh"
"$repository_root/scripts/test-compiler-fact-protocol.sh"
