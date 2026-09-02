#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/../.." && pwd -P)"
readonly repository_root

if [[ "$#" != 4 ]]; then
  echo "usage: $0 <archive> <target> <version> <surface:true|false>" >&2
  exit 2
fi

archive="$1"
target="$2"
version="$3"
surface="$4"
if [[ "$surface" != "true" && "$surface" != "false" ]]; then
  echo "surface must be true or false" >&2
  exit 2
fi

cd "$repository_root"
if [[ "$archive" != /* ]]; then
  archive="$repository_root/$archive"
fi
if [[ ! -f "$archive" ]]; then
  echo "release archive was not produced: $archive" >&2
  exit 1
fi

temporary_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
smoke="$(mktemp -d "$temporary_root/cargo-rail-release-smoke.XXXXXX")"
readonly smoke
cleanup() {
  rm -rf -- "$smoke"
}
trap cleanup EXIT

fail() {
  echo "release archive smoke test: $1" >&2
  exit 1
}

require_file() {
  local path="$1"
  local component="$2"
  [[ -n "$path" && -f "$path" ]] || fail "missing $component"
}

require_same_directory() {
  local binary="$1"
  local component="$2"
  [[ "$(dirname "$binary")" == "$(dirname "$component")" ]] ||
    fail "$(basename "$component") is not adjacent to $(basename "$binary")"
}

captured_output=""
captured_stderr=""
capture_surface() {
  local label="$1"
  local expected_status="$2"
  local toolchain="$3"
  local mode="$4"
  local stdout_file="$smoke/$label.stdout"
  local stderr_file="$smoke/$label.stderr"
  local status

  set +e
  if [[ -n "$toolchain" ]]; then
    (cd tests/fixtures/surface_release && RUSTUP_TOOLCHAIN="$toolchain" "$binary" rail surface "$mode" -f json) \
      >"$stdout_file" 2>"$stderr_file"
  else
    (cd tests/fixtures/surface_release && "$binary" rail surface "$mode" -f json) \
      >"$stdout_file" 2>"$stderr_file"
  fi
  status=$?
  set -e

  if [[ "$status" != "$expected_status" ]]; then
    echo "release archive smoke test: $label exited $status; expected $expected_status" >&2
    sed -n '1,240p' "$stdout_file" >&2
    sed -n '1,240p' "$stderr_file" >&2
    exit 1
  fi
  captured_output="$(<"$stdout_file")"
  captured_stderr="$stderr_file"
}

assert_json() {
  local label="$1"
  local document="$2"
  local filter="$3"
  local expected="$4"
  local actual

  if ! actual="$(printf '%s' "$document" | jq -r "$filter")"; then
    echo "release archive smoke test: $label is not valid expected JSON" >&2
    printf '%s\n' "$document" >&2
    [[ -z "$captured_stderr" ]] || sed -n '1,240p' "$captured_stderr" >&2
    exit 1
  fi
  if [[ "$actual" != "$expected" ]]; then
    echo "release archive smoke test: $label expected '$expected', got '$actual'" >&2
    printf '%s\n' "$document" >&2
    [[ -z "$captured_stderr" ]] || sed -n '1,240p' "$captured_stderr" >&2
    exit 1
  fi
}

toolchain_for_target() {
  local release="$1"
  if [[ "$target" == *-linux-musl ]]; then
    printf '%s-%s\n' "$release" "$target"
  else
    printf '%s\n' "$release"
  fi
}

install_toolchain_for_target() {
  local toolchain="$1"
  local arguments=(toolchain install "$toolchain" --profile minimal)
  if [[ "$target" == *-linux-musl ]]; then
    arguments+=(--force-non-host)
  fi
  rustup "${arguments[@]}"
}

tar -xzf "$archive" -C "$smoke"
binary="$(find "$smoke" -type f -name cargo-rail -print -quit)"
observation="$(find "$smoke" -type f -name cargo-rail-compiler-observation -print -quit)"
wrapper="$(find "$smoke" -type f -name cargo-rail-native-rustc-wrapper -print -quit)"
worker="$(find "$smoke" -type f -name cargo-rail-native-rustc-worker -print -quit)"
distributed_worker="$(find "$smoke" -type f -name cargo-rail-distributed-worker -print -quit)"
driver="$(find "$smoke" -type f -name cargo-rail-fact-driver -print -quit)"
driver_source="$(find "$smoke" -type f -name cargo-rail-fact-driver-source-v1.json -print -quit)"
component_manifest="$(find "$smoke" -type f -name cargo-rail-components-v1.tsv -print -quit)"

require_file "$binary" cargo-rail
require_file "$observation" cargo-rail-compiler-observation
require_file "$wrapper" cargo-rail-native-rustc-wrapper
require_file "$worker" cargo-rail-native-rustc-worker
require_file "$distributed_worker" cargo-rail-distributed-worker
require_file "$component_manifest" cargo-rail-components-v1.tsv
require_same_directory "$binary" "$observation"
require_same_directory "$binary" "$wrapper"
require_same_directory "$binary" "$worker"
require_same_directory "$binary" "$distributed_worker"

binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"
chmod +x "$binary" "$observation" "$wrapper" "$worker" "$distributed_worker"
scripts/ci/verify-release-components.py "$(dirname "$binary")" \
  --target "$target" \
  --version "$version" \
  --surface "$surface"
if [[ "$target" == *-unknown-linux-gnu ]]; then
  python3 scripts/ci/verify-gnu-runtime.py "$(dirname "$binary")" --target "$target"
fi

observation_protocol="$("$observation" --cargo-rail-observation-protocol-version)"
[[ "$observation_protocol" == "1" ]] ||
  fail "compiler observation protocol is '$observation_protocol'; expected '1'"

if [[ "$surface" == "false" ]]; then
  [[ -z "$driver" ]] || fail "surface-disabled archive contains cargo-rail-fact-driver"
  [[ -z "$driver_source" ]] || fail "surface-disabled archive contains compiler driver source"
  unavailable="$smoke/surface-unavailable"
  mkdir "$unavailable"
  set +e
  (cd "$unavailable" && "$binary" rail surface --check) >"$smoke/unavailable.stdout" 2>"$smoke/unavailable.stderr"
  unavailable_status=$?
  set -e
  [[ "$unavailable_status" != 0 ]] || fail "surface unexpectedly succeeded without its artifact"
  if ! grep -Fq "surface is unavailable" "$smoke/unavailable.stderr"; then
    sed -n '1,120p' "$smoke/unavailable.stderr" >&2
    fail "surface did not report its unavailable-artifact diagnostic"
  fi
else
  require_file "$driver" cargo-rail-fact-driver
  require_file "$driver_source" cargo-rail-fact-driver-source-v1.json
  require_same_directory "$binary" "$driver"
  require_same_directory "$binary" "$driver_source"
  chmod +x "$driver"

  : "${CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY:?compiler library authority is missing}"
  compiler_library="$(rustc --print sysroot)/$CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY"
  compiler_library_directory="$(dirname "$compiler_library")"
  if ! protocol="$(LD_LIBRARY_PATH="$compiler_library_directory${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    DYLD_LIBRARY_PATH="$compiler_library_directory${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
    "$driver" --cargo-rail-fact-protocol-version)"; then
    fail "compiler fact driver protocol probe failed"
  fi
  [[ "$protocol" =~ ^[1-9][0-9]*$ ]] || fail "compiler fact driver emitted invalid protocol '$protocol'"

  rustup component remove rustc-dev || true
  capture_surface stable-prepare 0 "" --prepare
  stable_preparation="$captured_output"
  assert_json "stable preparation mode" "$stable_preparation" '.mode' prepare
  assert_json "stable preparation result" "$stable_preparation" '.result' ready
  assert_json "stable preparation contract" "$stable_preparation" '.surface_preparation_contract_version' 1
  assert_json "stable driver identity" "$stable_preparation" \
    '.driver.identity | test("^compiler-fact-driver-v1-sha256-[0-9a-f]{64}$")' true

  # Readiness and analysis prove that rustc itself or rustc-dev supplied the
  # exact compiler library required by the authenticated driver.
  capture_surface stable-check 1 "" --check
  stable_report="$captured_output"
  assert_json "stable surface contract" "$stable_report" '.surface_contract_version' 3
  assert_json "stable audited targets" "$stable_report" '.authority.audited_targets | length > 0' true
  assert_json "stable dead-public finding" "$stable_report" '.findings | any(.name == "dead_public")' true

  if [[ "$target" == *-linux-musl ]]; then
    capture_surface stable-warm 1 "" --check
    stable_warm_report="$captured_output"
    assert_json "warm surface contract" "$stable_warm_report" '.surface_contract_version' 3
    assert_json "warm fact cache hits" "$stable_warm_report" '.metrics.acquisition.fact_cache_hits > 0' true
    assert_json "warm Cargo executions" "$stable_warm_report" '.metrics.acquisition.cargo_views_executed' 0
    assert_json "warm compiler invocations" "$stable_warm_report" '.metrics.acquisition.compiler_invocations' 0
    stable_objects="$(jq -c '.cache.compiler_fact_objects' <<<"$stable_report")"
    warm_objects="$(jq -c '.cache.compiler_fact_objects' <<<"$stable_warm_report")"
    [[ "$warm_objects" == "$stable_objects" ]] || fail "warm Surface changed compiler fact object identities"

    fallback_release="1.97.0"
    fallback_toolchain="$(toolchain_for_target "$fallback_release")"
    install_toolchain_for_target "$fallback_toolchain"
    rustup component remove rustc-dev --toolchain "$fallback_toolchain" || true
    capture_surface fallback-stable-prepare 0 "$fallback_toolchain" --prepare
    fallback_preparation="$captured_output"
    assert_json "fallback stable preparation result" "$fallback_preparation" '.result' ready
    assert_json "fallback stable preparation contract" "$fallback_preparation" \
      '.surface_preparation_contract_version' 1
    assert_json "fallback stable preparation toolchain" "$fallback_preparation" \
      ".toolchain.rustc | contains(\"$fallback_release\")" true
    fallback_driver_identity="$(jq -r '.driver.identity' <<<"$fallback_preparation")"
    stable_driver_identity="$(jq -r '.driver.identity' <<<"$stable_preparation")"
    [[ "$fallback_driver_identity" != "$stable_driver_identity" ]] ||
      fail "fallback stable toolchain reused the embedded driver identity"

    capture_surface fallback-stable-check 1 "$fallback_toolchain" --check
    fallback_report="$captured_output"
    assert_json "fallback stable surface contract" "$fallback_report" '.surface_contract_version' 3
    assert_json "fallback stable audited targets" "$fallback_report" '.authority.audited_targets | length > 0' true
    assert_json "fallback stable dead-public finding" "$fallback_report" \
      '.findings | any(.name == "dead_public")' true
  fi

  nightly_toolchain="$(toolchain_for_target nightly-2026-08-12)"
  install_toolchain_for_target "$nightly_toolchain"
  rustup component remove rustc-dev --toolchain "$nightly_toolchain" || true
  capture_surface nightly-prepare 0 "$nightly_toolchain" --prepare
  nightly_preparation="$captured_output"
  assert_json "nightly preparation mode" "$nightly_preparation" '.mode' prepare
  assert_json "nightly preparation result" "$nightly_preparation" '.result' ready
  assert_json "nightly preparation contract" "$nightly_preparation" '.surface_preparation_contract_version' 1
  assert_json "nightly preparation toolchain" "$nightly_preparation" '.toolchain.rustc | contains("1.99.0-nightly")' true

  capture_surface nightly-check 1 "$nightly_toolchain" --check
  nightly_report="$captured_output"
  assert_json "nightly surface contract" "$nightly_report" '.surface_contract_version' 3
  assert_json "nightly surface toolchain" "$nightly_report" '.toolchain.rustc | contains("1.99.0-nightly")' true
  assert_json "nightly audited targets" "$nightly_report" '.authority.audited_targets | length > 0' true
fi

python3 scripts/ci/verify-distributed-worker.py --worker "$distributed_worker" --rustc rustc
reported_version="$("$binary" rail --version)"
[[ "$reported_version" == "cargo-rail $version" ]] ||
  fail "cargo-rail version is '$reported_version'; expected 'cargo-rail $version'"
