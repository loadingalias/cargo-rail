#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/../.."
[[ "$#" -ge 1 && "$#" -le 2 ]] \
  || { echo 'usage: scripts/tooling/package-unix.sh TARGET [ci|package]' >&2; exit 64; }
operation="${2:-package}"
[[ "$operation" == ci || "$operation" == package ]] \
  || { echo "unsupported Unix tooling operation: $operation" >&2; exit 64; }
case "$1" in
  x86_64-unknown-linux-gnu) exec "$SCRIPT_DIR/x86_64-linux.sh" "$operation" ;;
  aarch64-unknown-linux-gnu) exec "$SCRIPT_DIR/aarch64-linux.sh" "$operation" ;;
  aarch64-apple-darwin) ;;
  *) echo "unsupported native package target: $1" >&2; exit 64 ;;
esac
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || { echo 'macOS packaging requires native Apple Silicon' >&2; exit 1; }
# The macOS runner supplies Xcode, CMake, Python, and rustup; check native prerequisites before installing Rust tools.
for tool in xcrun cc cmake python3 rustup cargo; do
  command -v "$tool" >/dev/null || { echo "missing macOS package prerequisite: $tool" >&2; exit 127; }
done
xcrun --find clang
cmake --version
python3 "$SCRIPT_DIR/catalog.py" validate
export RUSTUP_TOOLCHAIN
RUSTUP_TOOLCHAIN="$(python3 "$SCRIPT_DIR/catalog.py" rust-channel)"
components=()
while IFS= read -r component; do components+=(--component "$component"); done < <(
  python3 "$SCRIPT_DIR/catalog.py" get operations "$operation" components
)
rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal "${components[@]}"
[[ "$(rustc -vV | sed -n 's/^host: //p')" == "$1" ]]
cargo install cargo-binstall --version "$(python3 "$SCRIPT_DIR/catalog.py" get versions cargo-binstall)" --locked
while IFS= read -r tool; do
  cargo binstall --locked --no-confirm --targets "$1" --strategies crate-meta-data \
    "$tool@$(python3 "$SCRIPT_DIR/catalog.py" get cargo "$tool")"
done < <(python3 "$SCRIPT_DIR/catalog.py" get operations "$operation" cargo)
if [[ -n "${GITHUB_ENV:-}" ]]; then printf 'RUSTUP_TOOLCHAIN=%s\n' "$RUSTUP_TOOLCHAIN" >> "$GITHUB_ENV"; fi
