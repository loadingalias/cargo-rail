#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" != 1 || -z "$1" || "$1" == *$'\n'* || "$1" == *$'\r'* ]]; then
  echo "usage: $0 <installed-rustup-toolchain>" >&2
  exit 2
fi
: "${GITHUB_ENV:?GitHub Actions environment file is not available}"

toolchain="$1"
{
  printf 'RUSTUP_TOOLCHAIN=%s\n' "$toolchain"
  printf 'RUSTUP_AUTO_INSTALL=0\n'
} >> "$GITHUB_ENV"

export RUSTUP_TOOLCHAIN="$toolchain"
export RUSTUP_AUTO_INSTALL=0
rustup which --toolchain "$toolchain" rustc >/dev/null
rustup which --toolchain "$toolchain" cargo >/dev/null
rustc --version --verbose
cargo --version
