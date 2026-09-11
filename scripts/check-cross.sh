#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

[[ "$#" -eq 0 ]] || { echo 'usage: scripts/check-cross.sh' >&2; exit 64; }
[[ "$(uname -s)" == Darwin ]] || { echo 'cross checks require the macOS workstation' >&2; exit 1; }
for tool in zig cargo-zigbuild cargo-xwin; do
  command -v "$tool" >/dev/null || { echo "missing cross check prerequisite: $tool" >&2; exit 127; }
done

args=(--workspace --all-targets --all-features --locked)
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
              x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  echo "Clippy: $target"
  cargo-zigbuild clippy --target "$target" "${args[@]}"
done
for target in x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
  echo "Clippy: $target"
  cargo xwin clippy --target "$target" "${args[@]}"
done
