#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"

usage() {
  echo "usage: $0 [--toolchain <major.minor.patch>] [--host <target-triple>] [--components <comma-list>] [--targets <comma-list>]" >&2
  exit 2
}

toolchain=""
host=""
component_list=""
target_list=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --toolchain)
      [[ "$#" -ge 2 ]] || usage
      toolchain="$2"
      shift 2
      ;;
    --host)
      [[ "$#" -ge 2 ]] || usage
      host="$2"
      shift 2
      ;;
    --components)
      [[ "$#" -ge 2 ]] || usage
      component_list="$2"
      shift 2
      ;;
    --targets)
      [[ "$#" -ge 2 ]] || usage
      target_list="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

if [[ -z "$toolchain" ]]; then
  toolchain="$(sed -n 's/^channel = "\([^"]*\)"[[:space:]]*$/\1/p' "$repository_root/rust-toolchain.toml")"
  [[ -n "$toolchain" && "$toolchain" != *$'\n'* ]] || {
    echo "rust-toolchain.toml must declare exactly one channel" >&2
    exit 2
  }
fi

[[ "$toolchain" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || usage
[[ -z "$host" || "$host" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || usage
: "${GITHUB_ENV:?GitHub Actions environment file is not available}"
: "${RUNNER_TEMP:?GitHub Actions runner temporary directory is not available}"
command -v rustup >/dev/null 2>&1 || {
  echo "GitHub Actions runner does not provide rustup" >&2
  exit 2
}

append_list() {
  local value="$1"
  local kind="$2"
  local option="$3"
  local items=()
  [[ -z "$value" ]] && return
  [[ "$value" != ,* && "$value" != *, && "$value" != *,,* ]] || {
    echo "invalid Rust $kind list: $value" >&2
    exit 2
  }
  IFS=',' read -r -a items <<<"$value"
  for item in "${items[@]}"; do
    [[ "$item" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || {
      echo "invalid Rust $kind: $item" >&2
      exit 2
    }
    [[ "$option" != --component || "$item" != cargo ]] || continue
    install+=("$option" "$item")
  done
}

# Runner images contain mutable, preinstalled Rustup state. A job-private home
# makes the exact toolchain and component inventory independent of that image.
rustup_home="$RUNNER_TEMP/cargo-rail-rustup"
mkdir -p "$rustup_home"
export RUSTUP_HOME="$rustup_home"

install_name="$toolchain"
if [[ -n "$host" ]]; then
  install_name="$toolchain-$host"
fi
install=(toolchain install "$install_name" --profile minimal --no-self-update --component cargo)
if [[ -n "$host" ]]; then
  install+=(--force-non-host)
fi
append_list "$component_list" component --component
append_list "$target_list" target --target
rustup "${install[@]}"

rustc_verbose="$(rustup run "$install_name" rustc --version --verbose)"
release="$(sed -n 's/^release: //p' <<<"$rustc_verbose")"
installed_host="$(sed -n 's/^host: //p' <<<"$rustc_verbose")"
[[ "$release" == "$toolchain" && "$installed_host" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || {
  echo "installed rustc does not match requested toolchain $toolchain" >&2
  exit 1
}
if [[ -n "$host" && "$installed_host" != "$host" ]]; then
  echo "installed rustc host $installed_host does not match requested host $host" >&2
  exit 1
fi
selected="$toolchain-$installed_host"

export RUSTUP_TOOLCHAIN="$selected"
export RUSTUP_AUTO_INSTALL=0
rustup which --toolchain "$selected" cargo >/dev/null
rustup run "$selected" cargo --version >/dev/null
for program in rustc rustdoc; do
  rustup which --toolchain "$selected" "$program" >/dev/null
  rustup run "$selected" "$program" --version >/dev/null
done

{
  printf 'RUSTUP_HOME=%s\n' "$RUSTUP_HOME"
  printf 'RUSTUP_TOOLCHAIN=%s\n' "$RUSTUP_TOOLCHAIN"
  printf 'RUSTUP_AUTO_INSTALL=0\n'
} >>"$GITHUB_ENV"

printf '%s\n' "$rustc_verbose"
rustup run "$selected" cargo --version
