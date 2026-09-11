#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

[[ "$#" -eq 0 ]] || { echo 'usage: scripts/check.sh' >&2; exit 64; }

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked
cargo deny --locked --workspace --all-features check -D warnings all
RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings" cargo doc --workspace --no-deps --all-features --locked
scripts/check-compiler-fact-driver.sh
