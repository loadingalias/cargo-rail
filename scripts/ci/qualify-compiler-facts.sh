#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mode="${1:-}"
runs="${2:-}"
run_id="${3:-}"
target="${DEV_MACHINE_TARGET:-}"

usage() {
  echo "usage: $0 <smoke|run> <runs> <run-id>" >&2
  exit 2
}

case "$mode" in
  smoke | run) ;;
  *) usage ;;
esac
[[ "$runs" =~ ^[0-9]+$ && "$runs" -gt 0 ]] || usage
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage

case "$target" in
  aws-linux-x64-bench)
    expected_host=x86_64-unknown-linux-gnu
    expected_filesystem=ext4
    expected_case_sensitive=true
    ;;
  aws-linux-arm64-bench)
    expected_host=aarch64-unknown-linux-gnu
    expected_filesystem=ext4
    expected_case_sensitive=true
    ;;
  aws-windows-x64-bench)
    expected_host=x86_64-pc-windows-msvc
    expected_filesystem=ntfs
    expected_case_sensitive=false
    ;;
  azure-windows-arm64-profile)
    expected_host=aarch64-pc-windows-msvc
    expected_filesystem=ntfs
    expected_case_sensitive=false
    ;;
  *)
    echo "compiler-fact qualification does not authorize target '$target'" >&2
    exit 2
    ;;
esac

results="$repo_root/benchmark_results/compiler-facts/$run_id"
[[ ! -e "$results" ]] || {
  echo "compiler-fact qualification result already exists: $results" >&2
  exit 2
}
mkdir -p "$results"
log="$results/correctness.log"

run_logged() {
  printf '\n$'
  printf ' %q' "$@"
  printf '\n'
  set +e
  "$@" 2>&1 | tee -a "$log"
  status="${PIPESTATUS[0]}"
  set -e
  [[ "$status" -eq 0 ]] || return "$status"
}

python_command=python3
if ! command -v "$python_command" >/dev/null 2>&1; then
  python_command=python
fi
command -v "$python_command" >/dev/null 2>&1 || {
  echo "compiler-fact qualification requires Python 3" >&2
  exit 2
}

cd "$repo_root"
run_logged rustup component add rustc-dev rust-src
run_logged rustup target add thumbv7em-none-eabihf wasm32-unknown-unknown wasm32-wasip1 wasm32v1-none
run_logged cargo build --locked --bins

cargo_rail=target/debug/cargo-rail
if [[ "${OS:-}" == Windows_NT ]]; then
  cargo_rail="$cargo_rail.exe"
fi
rustc_release="$(rustc -vV | sed -n 's/^release: //p')"
run_logged "$python_command" scripts/ci/run-filesystem-compatibility.py \
  --root "${TMPDIR:-${TEMP:-/tmp}}" \
  --expected-filesystem "$expected_filesystem" \
  --expected-case-sensitive "$expected_case_sensitive"
run_logged "$python_command" scripts/ci/run-compatibility.py \
  --cargo-rail "$cargo_rail" \
  --toolchain "$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)" \
  --expected-rust-release "$rustc_release" \
  --expected-host "$expected_host" \
  --selection-probes \
  --cross-target-mutation-probes
run_logged just check-compiler-fact-driver
run_logged scripts/with-compiler-fact-driver.sh just build-all
run_logged scripts/with-compiler-fact-driver.sh just test-all

export CARGO_RAIL_COMPILER_FACT_RESULTS="$results"
if [[ "$mode" == smoke ]]; then
  run_logged "$python_command" scripts/bench/compiler-facts.py smoke
else
  run_logged "$python_command" scripts/bench/compiler-facts.py run "$runs"
fi

printf '{"schema_version":1,"status":"passed","expected_host":"%s","target":"%s"}\n' \
  "$expected_host" "$target" >"$results/correctness.json"
