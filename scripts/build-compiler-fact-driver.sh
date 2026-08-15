#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root
readonly manifest="$repository_root/tools/compiler-fact-driver/Cargo.toml"

# The unstable compiler API authority applies to this one named companion
# crate and this one manufacturing process. `env` does not modify the caller,
# and the companion is excluded from every ordinary cargo-rail workspace build.
env RUSTC_BOOTSTRAP=cargo_rail_fact_driver \
  cargo build --locked --release --manifest-path "$manifest" "$@"
