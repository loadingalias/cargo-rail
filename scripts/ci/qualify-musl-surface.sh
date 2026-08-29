#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
readonly repository_root
run_id="${1:-}"
remote_target="${DEV_MACHINE_TARGET:-}"

usage() {
  echo "usage: $0 <run-id> (on aws-linux-x64 or aws-linux-arm64)" >&2
  exit 2
}

[[ "$#" -eq 1 ]] || usage
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
case "$remote_target" in
  aws-linux-x64)
    target=x86_64-unknown-linux-musl
    expected_native_host=x86_64-unknown-linux-gnu
    ;;
  aws-linux-arm64)
    target=aarch64-unknown-linux-musl
    expected_native_host=aarch64-unknown-linux-gnu
    ;;
  *)
    echo "musl Surface qualification does not authorize target '$remote_target'" >&2
    exit 2
    ;;
esac
readonly target expected_native_host

results_root="$repository_root/benchmark_results"
results="$results_root/musl-surface/$run_id"
transfer_root="$results_root/.transfers"
archive="$transfer_root/$run_id.tar"
manifest="$archive.sha256"
[[ ! -e "$results" && ! -e "$archive" && ! -e "$manifest" ]] || {
  echo "musl Surface qualification result already exists: $run_id" >&2
  exit 2
}
mkdir -p "$results" "$transfer_root"
log="$results/qualification.log"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-musl-qualification.XXXXXX")"
environment_file="$temporary/github-env"
output_file="$temporary/github-output"
touch "$environment_file" "$output_file"
status=failed
failure_status=1

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

finalize() {
  local command_status="$?"
  set +e
  failure_status="$command_status"
  if [[ ! -f "$results/result.json" ]]; then
    jq -n \
      --arg status "$status" \
      --arg target "$target" \
      --arg remote_target "$remote_target" \
      --argjson exit_status "$failure_status" \
      '{schema_version: 1, status: $status, target: $target, remote_target: $remote_target, exit_status: $exit_status}' \
      >"$results/result.json"
  fi
  rm -rf -- "$temporary"

  archive_staging="$transfer_root/.$run_id.tar.staging"
  manifest_staging="$transfer_root/.$run_id.tar.sha256.staging"
  rm -f -- "$archive_staging" "$manifest_staging"
  tar -cf "$archive_staging" -C "$results_root" "musl-surface/$run_id"
  transfer_digest="$(sha256_file "$archive_staging")"
  mv "$archive_staging" "$archive"
  printf '%s  %s\n' "$transfer_digest" "$run_id.tar" >"$manifest_staging"
  mv "$manifest_staging" "$manifest"
}
trap finalize EXIT

exec > >(tee -a "$log") 2>&1
cd "$repository_root"

echo "musl Surface qualification: remote=$remote_target target=$target run=$run_id"
for tool in cargo jq python3 rustc rustup sudo tar; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "musl Surface qualification requires $tool" >&2
    exit 2
  }
done
[[ -r /etc/os-release ]] || {
  echo "musl Surface qualification requires Linux distribution identity" >&2
  exit 2
}
# shellcheck disable=SC1091
source /etc/os-release
[[ "${ID:-}" == ubuntu ]] || {
  echo "musl Surface qualification requires the qualified Ubuntu runner image" >&2
  exit 2
}
native_host="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$native_host" == "$expected_native_host" ]] || {
  echo "native Rust host '$native_host' does not match '$expected_native_host'" >&2
  exit 1
}

toolchain="$(sed -n 's/^channel = "\([^"]*\)"[[:space:]]*$/\1/p' rust-toolchain.toml)"
[[ "$toolchain" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "rust-toolchain.toml does not declare one exact stable toolchain" >&2
  exit 1
}
version="$(cargo metadata --locked --format-version 1 --no-deps \
  | jq -er '[.packages[] | select(.name == "cargo-rail") | .version] | if length == 1 then .[0] else error("cargo-rail package identity is not unique") end')"

export GITHUB_ENV="$environment_file"
export RUNNER_TEMP="$temporary/runner"
mkdir -p "$RUNNER_TEMP"
scripts/ci/install-musl-toolchain.sh "$target" "$environment_file"
set -a
# shellcheck disable=SC1090
source "$environment_file"
set +a
scripts/ci/install-rust-toolchain.sh \
  --toolchain "$toolchain" \
  --host "$target" \
  --components rustc-dev
set -a
# The installer emits shell-safe absolute paths and exact Rustup selectors.
# shellcheck disable=SC1090
source "$environment_file"
set +a
[[ "$(rustc -vV | sed -n 's/^host: //p')" == "$target" ]] || {
  echo "selected Rust host does not match archive target $target" >&2
  exit 1
}

component_directory="$temporary/components"
mkdir -p "$component_directory"
scripts/ci/manufacture-compiler-fact-driver.sh \
  "$target" "$environment_file" "$output_file" "$component_directory"
set -a
# shellcheck disable=SC1090
source "$environment_file"
set +a
[[ "$(sed -n 's/^include=//p' "$output_file")" == \
  "cargo-rail-fact-driver,cargo-rail-fact-driver-source-v1.json" ]] || {
  echo "compiler fact driver manufacturing emitted an unexpected component inventory" >&2
  exit 1
}

remap="--remap-path-prefix=$repository_root=/workspace --remap-path-scope=object"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$remap -C target-feature=+crt-static -C link-self-contained=yes"
unset CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
build_root="$temporary/build"
cargo build \
  --locked \
  --release \
  --target "$target" \
  --target-dir "$build_root" \
  --bin cargo-rail \
  --bin cargo-rail-compiler-observation \
  --bin cargo-rail-native-rustc-wrapper \
  --bin cargo-rail-native-rustc-worker \
  --bin cargo-rail-distributed-worker

archive_root="$temporary/archive/cargo-rail-$target"
mkdir -p "$archive_root"
for binary in \
  cargo-rail \
  cargo-rail-compiler-observation \
  cargo-rail-native-rustc-wrapper \
  cargo-rail-native-rustc-worker \
  cargo-rail-distributed-worker; do
  install -m 755 "$build_root/$target/release/$binary" "$archive_root/$binary"
done
install -m 755 "$component_directory/cargo-rail-fact-driver" \
  "$archive_root/cargo-rail-fact-driver"
install -m 600 "$component_directory/cargo-rail-fact-driver-source-v1.json" \
  "$archive_root/cargo-rail-fact-driver-source-v1.json"
release_archive="$temporary/cargo-rail-$target.tar.gz"
tar -czf "$release_archive" -C "$temporary/archive" "cargo-rail-$target"
scripts/package-release-archive.py "$release_archive" \
  --target "$target" \
  --version "$version" \
  --surface true
scripts/ci/smoke-release-tar.sh "$release_archive" "$target" "$version" true

archive_digest="$(sha256_file "$release_archive")"
archive_bytes="$(wc -c <"$release_archive" | tr -d ' ')"
driver_protocol="$(
  compiler_library="$(rustc --print sysroot)/$CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY"
  LD_LIBRARY_PATH="$(dirname "$compiler_library")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    "$component_directory/cargo-rail-fact-driver" \
      --cargo-rail-fact-protocol-version
)"
[[ "$driver_protocol" == 4 ]] || {
  echo "qualified compiler fact driver protocol is '$driver_protocol'; expected '4'" >&2
  exit 1
}

status=passed
jq -n \
  --arg status "$status" \
  --arg target "$target" \
  --arg remote_target "$remote_target" \
  --arg native_host "$native_host" \
  --arg toolchain "$toolchain" \
  --arg musl_toolchain_image "$CARGO_RAIL_MUSL_TOOLCHAIN_IMAGE" \
  --arg version "$version" \
  --arg archive_sha256 "$archive_digest" \
  --argjson archive_bytes "$archive_bytes" \
  --argjson compiler_fact_protocol "$driver_protocol" \
  '{
    schema_version: 1,
    status: $status,
    target: $target,
    remote_target: $remote_target,
    native_host: $native_host,
    toolchain: $toolchain,
    musl_toolchain_image: $musl_toolchain_image,
    cargo_rail_version: $version,
    archive_sha256: $archive_sha256,
    archive_bytes: $archive_bytes,
    compiler_fact_protocol: $compiler_fact_protocol
  }' >"$results/result.json"
cat "$results/result.json"
