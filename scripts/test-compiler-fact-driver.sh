#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root
readonly manifest="$repository_root/tools/compiler-fact-driver/Cargo.toml"

sysroot="$(rustc --print sysroot)"
if command -v cygpath >/dev/null 2>&1; then
  sysroot="$(cygpath -u "$sysroot")"
fi
if [[ "${OS:-}" == Windows_NT ]]; then
  export PATH="$sysroot/bin:$PATH"
else
  export LD_LIBRARY_PATH="$sysroot/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

env RUSTC_BOOTSTRAP=cargo_rail_fact_driver \
  cargo test --locked --manifest-path "$manifest" "$@"
