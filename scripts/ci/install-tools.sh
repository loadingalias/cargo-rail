#!/usr/bin/env bash
set -euo pipefail

profile="${1:-}"
case "$profile" in
  linux-* | windows-*) ;;
  *)
    echo "usage: $0 <linux-*|windows-* dev-machines profile>" >&2
    exit 2
    ;;
esac

readonly CARGO_NEXTEST_VERSION=0.9.140
readonly CARGO_DENY_VERSION=0.20.2
readonly CARGO_AUDIT_VERSION=0.22.2
readonly HYPERFINE_VERSION=1.20.0
readonly JUST_VERSION=1.57.0
readonly SCCACHE_VERSION=0.16.0
readonly JQ_VERSION=1.8.2

cargo_bin="$HOME/.cargo/bin"
mkdir -p "$cargo_bin"
export PATH="$cargo_bin:$PATH"

# dev-machines installs the remote cache environment before this script. The
# wrapper binary does not exist until sccache is installed below.
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER

install_cargo_tool() {
  local package="$1"
  local version="$2"
  local binary="${3:-$package}"
  local path="$cargo_bin/$binary"
  [[ -x "$path" ]] || path="$path.exe"
  if [[ -x "$path" ]] && "$path" --version 2>&1 | grep -Fq "$version"; then
    echo "$package $version is already installed"
    return
  fi
  cargo install --registry crates-io --locked --version "=$version" --force "$package"
  [[ -x "$cargo_bin/$binary" || -x "$cargo_bin/$binary.exe" ]] || {
    echo "$package did not install $binary" >&2
    exit 1
  }
}

install_jq() {
  if command -v jq >/dev/null 2>&1 && [[ "$(jq --version)" == "jq-$JQ_VERSION" ]]; then
    echo "jq $JQ_VERSION is already installed"
    return
  fi

  local asset digest destination
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64 | Linux:amd64)
      asset="jq-linux-amd64"
      digest="b1c22172dd303f3be49e935aa56aa48a8b7a46e0bc838b4997d3bb451495870f"
      destination="$cargo_bin/jq"
      ;;
    Linux:aarch64 | Linux:arm64)
      asset="jq-linux-arm64"
      digest="8b85c817833814ddca00a144c33705546355afccf0cf39b188f3cdb48b852309"
      destination="$cargo_bin/jq"
      ;;
    MINGW*:x86_64 | MSYS*:x86_64 | CYGWIN*:x86_64 | MINGW*:amd64 | MSYS*:amd64 | CYGWIN*:amd64)
      asset="jq-windows-amd64.exe"
      digest="a6fc67fedaf9128a3309a1e2ebb8b986aeccf70122ee46d2cb4849e423f0c627"
      destination="$cargo_bin/jq.exe"
      ;;
    MINGW*:aarch64 | MSYS*:aarch64 | CYGWIN*:aarch64 | MINGW*:arm64 | MSYS*:arm64 | CYGWIN*:arm64)
      asset="jq-windows-arm64.exe"
      digest="083b5377392bc57cf27052b6d20a2d927770683bca844632901ff38b4b7b0ac7"
      destination="$cargo_bin/jq.exe"
      ;;
    *)
      echo "jq $JQ_VERSION has no configured asset for $(uname -s) $(uname -m)" >&2
      exit 1
      ;;
  esac
  local temporary
  temporary="$(mktemp "${TMPDIR:-/tmp}/jq.XXXXXX")"
  trap 'rm -f -- "$temporary"' RETURN
  curl --proto '=https' --tlsv1.2 -fsSL \
    "https://github.com/jqlang/jq/releases/download/jq-$JQ_VERSION/$asset" \
    -o "$temporary"
  printf '%s  %s\n' "$digest" "$temporary" | sha256sum --check --status
  mv "$temporary" "$destination"
  chmod +x "$destination"
  trap - RETURN
}

install_cargo_tool just "$JUST_VERSION"
install_cargo_tool cargo-nextest "$CARGO_NEXTEST_VERSION" cargo-nextest
install_cargo_tool cargo-deny "$CARGO_DENY_VERSION" cargo-deny
install_cargo_tool cargo-audit "$CARGO_AUDIT_VERSION" cargo-audit
install_cargo_tool hyperfine "$HYPERFINE_VERSION"
install_cargo_tool sccache "$SCCACHE_VERSION"
install_jq

just --version
cargo nextest --version
hyperfine --version
jq --version
sccache --version
