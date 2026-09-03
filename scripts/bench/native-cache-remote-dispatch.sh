#!/usr/bin/env bash
set -euo pipefail

# Run the native correctness and performance contract on one qualification host.

usage() {
  echo "usage: $0 <smoke|run> <runs> <run-id>" >&2
  exit 2
}

mode="${1:-}"
runs="${2:-}"
run_id="${3:-}"
target="${DEV_MACHINE_TARGET:-}"

case "$mode" in
  smoke) [[ "$runs" == 1 ]] || usage ;;
  run) [[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 20 && "$runs" -le 30 ]] || usage ;;
  *) usage ;;
esac
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "$script_dir/../.." && pwd -P)"
results="$repo_root/benchmark_results/native-cache/$run_id"
[[ ! -e "$results" ]] || {
  echo "native-cache host qualification result already exists: $results" >&2
  exit 2
}

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
  *)
    echo "native-cache host qualification does not authorize target '$target'" >&2
    exit 2
    ;;
esac

python_command=python3
if ! command -v "$python_command" >/dev/null 2>&1; then
  python_command=python
fi
for tool in cargo just jq rustc "$python_command"; do
  command -v "$tool" >/dev/null || {
    echo "native-cache host qualification requires $tool" >&2
    exit 2
  }
done

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-task9-native.XXXXXX")"
cleanup() {
  rm -rf -- "$temporary"
}
trap cleanup EXIT
correctness_log="$temporary/correctness.log"

run_logged() {
  printf '\n$' | tee -a "$correctness_log"
  printf ' %q' "$@" | tee -a "$correctness_log"
  printf '\n' | tee -a "$correctness_log"
  set +e
  "$@" 2>&1 | tee -a "$correctness_log"
  local command_status="${PIPESTATUS[0]}"
  set -e
  return "$command_status"
}

run_correctness() {
  cd "$repo_root"
  run_logged cargo build --locked --bins || return
  local cargo_rail=target/debug/cargo-rail
  if [[ "${OS:-}" == Windows_NT ]]; then
    cargo_rail="$cargo_rail.exe"
  fi
  local rustc_release
  rustc_release="$(rustc -vV | sed -n 's/^release: //p')"
  run_logged "$python_command" scripts/ci/run-filesystem-compatibility.py \
    --root "${TMPDIR:-${TEMP:-/tmp}}" \
    --expected-filesystem "$expected_filesystem" \
    --expected-case-sensitive "$expected_case_sensitive" || return
  run_logged "$python_command" scripts/ci/run-compatibility.py \
    --cargo-rail "$cargo_rail" \
    --toolchain "$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)" \
    --expected-rust-release "$rustc_release" \
    --expected-host "$expected_host" \
    --selection-probes \
    --cross-target-mutation-probes || return
  run_logged just check-compiler-fact-driver || return
  run_logged scripts/with-compiler-fact-driver.sh just build-all || return
  run_logged scripts/with-compiler-fact-driver.sh just test-all || return
  run_logged scripts/with-compiler-fact-driver.sh cargo test --doc -p cargo-rail --locked
}

correctness_status=0
run_correctness || correctness_status=$?

benchmark_status=2
if [[ "$correctness_status" -eq 0 ]]; then
  benchmark_status=0
  if [[ "$mode" == smoke ]]; then
    CARGO_RAIL_BENCH_INSTANCE_TYPE="${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
      CARGO_RAIL_BENCH_RESULTS="$results" "$script_dir/native-cache.sh" smoke || benchmark_status=$?
  else
    CARGO_RAIL_BENCH_INSTANCE_TYPE="${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
      CARGO_RAIL_BENCH_RESULTS="$results" "$script_dir/native-cache.sh" run "$runs" || benchmark_status=$?
  fi
fi

mkdir -p "$results/platform"
cp "$correctness_log" "$results/platform/correctness.log"
for harness in \
  scripts/bench/native-cache-remote-dispatch.sh \
  scripts/ci/run-filesystem-compatibility.py \
  scripts/ci/run-compatibility.py \
  scripts/test/test.sh \
  scripts/with-compiler-fact-driver.sh; do
  printf '%s  %s\n' "$(sha256_file "$repo_root/$harness")" "$harness"
done >"$results/platform/harness-sha256.txt"
summary_present=false
performance_qualified=false
environment_sha256=""
summary_sha256=""
if [[ -s "$results/environment.json" ]]; then
  environment_sha256="$(sha256_file "$results/environment.json")"
fi
if [[ -s "$results/summary.json" ]]; then
  summary_present=true
  performance_qualified="$(jq -r '.performance_qualified == true' "$results/summary.json")"
  summary_sha256="$(sha256_file "$results/summary.json")"
fi
qualified=false
if [[ "$mode" == run && "$correctness_status" -eq 0 && "$benchmark_status" -eq 0 \
  && "$performance_qualified" == true ]]; then
  qualified=true
fi
jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg run_id "$run_id" \
  --arg mode "$mode" \
  --arg target "$target" \
  --arg expected_host "$expected_host" \
  --arg expected_filesystem "$expected_filesystem" \
  --argjson expected_case_sensitive "$expected_case_sensitive" \
  --argjson correctness_status "$correctness_status" \
  --argjson benchmark_status "$benchmark_status" \
  --argjson summary_present "$summary_present" \
  --argjson performance_qualified "$performance_qualified" \
  --argjson qualified "$qualified" \
  --arg repository_commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --arg correctness_log_sha256 "$(sha256_file "$results/platform/correctness.log")" \
  --arg platform_harness_sha256 "$(sha256_file "$results/platform/harness-sha256.txt")" \
  --arg environment_sha256 "$environment_sha256" \
  --arg summary_sha256 "$summary_sha256" '
  {
    schema_version: 1,
    generated_at: $generated_at,
    run_id: $run_id,
    mode: $mode,
    target: $target,
    expected_host: $expected_host,
    expected_filesystem: $expected_filesystem,
    expected_case_sensitive: $expected_case_sensitive,
    correctness_passed: $correctness_status == 0,
    benchmark_evidence_passed: $benchmark_status == 0,
    summary_present: $summary_present,
    performance_qualified: $performance_qualified,
    qualified: $qualified,
    repository_commit: $repository_commit,
    correctness_log_sha256: $correctness_log_sha256,
    platform_harness_sha256: $platform_harness_sha256,
    benchmark_environment_sha256:
      (if $environment_sha256 == "" then null else $environment_sha256 end),
    benchmark_summary_sha256: (if $summary_sha256 == "" then null else $summary_sha256 end)
  }
' >"$results/platform/qualification.json"

[[ "$correctness_status" -eq 0 ]] || exit "$correctness_status"
[[ "$benchmark_status" -eq 0 ]] || exit "$benchmark_status"
if [[ "$mode" == run && "$qualified" != true ]]; then
  echo "native-cache host qualification did not pass the complete performance contract" >&2
  exit 1
fi
echo "native-cache host qualification result: $results"
