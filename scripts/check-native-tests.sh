#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# These runners have no prebuilt nextest. Bound concurrent Cargo subprocesses.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
target_directory="$(
  cargo metadata --no-deps --format-version 1 --locked --offline |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
)"
component_directory="$target_directory/debug"
scripts/check-source-installation.sh --prepare "$component_directory"
# shellcheck source=/dev/null
source "$component_directory/source-installation-authority.env"
scripts/check-compiler-fact-driver.sh --prepare "$component_directory"
# The preparation step generates this exact component authority.
# shellcheck source=/dev/null
source "$component_directory/compiler-driver-authority.env"
# Fixture Cargo commands must not inherit the runner's build directory.
unset CARGO_TARGET_DIR
cargo test --target-dir "$target_directory" --workspace --all-targets --all-features --locked --no-fail-fast -- --test-threads=2
cargo test --target-dir "$target_directory" --workspace --doc --all-features --locked
