#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
reporter="$repo_root/scripts/bench/native-cache-report.sh"
measure="$repo_root/scripts/bench/measure-command.py"
coverage="$repo_root/scripts/bench/native-cache-coverage.py"
timing_wrapper="$repo_root/scripts/bench/rustc-timing-wrapper.sh"

usage() {
  echo "usage: $0 <20-30 runs>|run <20-30 runs>|smoke|resume <result-directory>" >&2
  exit 2
}

operation="${1:-}"
case "$operation" in
  run)
    [[ "$#" -eq 2 ]] || usage
    runs="$2"
    evidence_kind=retained
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
  smoke)
    [[ "$#" -eq 1 ]] || usage
    runs=1
    evidence_kind=smoke
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/smoke-$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
  resume)
    [[ "$#" -eq 2 ]] || usage
    results="$(cd "$2" 2>/dev/null && pwd -P)" || {
      echo "native-cache result directory does not exist: $2" >&2
      exit 2
    }
    [[ -f "$results/run.json" ]] || {
      echo "native-cache result is not resumable because run.json is missing: $results" >&2
      exit 2
    }
    runs="$(jq -er '.required_accepted_samples' "$results/run.json")"
    evidence_kind="$(jq -er '.evidence_kind' "$results/run.json")"
    ;;
  '' | *[!0-9]*) usage ;;
  *)
    [[ "$#" -eq 1 ]] || usage
    runs="$operation"
    operation=run
    evidence_kind=retained
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
esac

if [[ "$operation" == resume ]]; then
  [[ "$runs" =~ ^[0-9]+$ && "$runs" -gt 0 ]] || {
    echo "native-cache benchmark evidence has an invalid sample count" >&2
    exit 2
  }
elif [[ "$evidence_kind" == retained ]]; then
  [[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 20 && "$runs" -le 30 ]] || {
    echo "retained native-cache qualification requires 20-30 accepted samples per lane" >&2
    exit 2
  }
fi
for tool in cargo git jq python3 sccache; do
  command -v "$tool" >/dev/null || {
    echo "missing required benchmark tool: $tool" >&2
    exit 2
  }
done

expected_sccache="$(sed -n 's/^readonly SCCACHE_VERSION=//p' "$repo_root/scripts/ci/install-tools.sh")"
sccache_bin="${SCCACHE_BIN:-$(command -v sccache)}"
sccache_version="$("$sccache_bin" --version)"
[[ "$sccache_version" == "sccache $expected_sccache" ]] || {
  echo "native-cache benchmark requires pinned sccache $expected_sccache; found $sccache_version" >&2
  exit 2
}

benchmark_lock="$repo_root/target/benchmarks/.native-cache.lock"
mkdir -p "$(dirname "$benchmark_lock")"
if ! mkdir "$benchmark_lock" 2>/dev/null; then
  owner="$(cat "$benchmark_lock/pid" 2>/dev/null || true)"
  echo "native-cache benchmark is already running${owner:+ as PID $owner}: $benchmark_lock" >&2
  exit 2
fi
printf '%s\n' "$$" >"$benchmark_lock/pid"

release_benchmark_lock() {
  [[ "$(cat "$benchmark_lock/pid" 2>/dev/null || true)" == "$$" ]] || return
  rm -rf -- "$benchmark_lock"
}
trap release_benchmark_lock EXIT

binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
if [[ -z "${CARGO_RAIL_BIN+x}" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
fi
[[ -x "$binary" ]] || {
  echo "cargo-rail benchmark binary is not executable: $binary" >&2
  exit 2
}
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

capture_worktree_patch() {
  local output="$1"
  git -C "$repo_root" diff --binary HEAD -- >"$output"
  while IFS= read -r -d '' untracked; do
    [[ -f "$repo_root/$untracked" || -L "$repo_root/$untracked" ]] || {
      echo "native-cache evidence cannot capture non-file untracked input: $untracked" >&2
      return 1
    }
    local status=0
    git -C "$repo_root" diff --binary --no-index -- /dev/null "$untracked" >>"$output" || status=$?
    [[ "$status" -eq 1 ]] || {
      echo "native-cache evidence could not capture untracked input: $untracked" >&2
      return 1
    }
  done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
}

if [[ "$operation" != resume ]]; then
  mkdir -p "$results"
  results="$(cd "$results" && pwd -P)"
  [[ -z "$(find "$results" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
    echo "native-cache result directory is not empty: $results" >&2
    exit 2
  }
fi

state="$results/state"
mkdir -p "$state"
sccache_runtime="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-sccache.XXXXXX")"
sccache_uds_supported=true
case "$(uname -s)" in
  CYGWIN* | MINGW* | MSYS*) sccache_uds_supported=false ;;
esac

stop_sccache_lane() {
  local lane="$1"
  local cache="$state/homes/$lane/cache"
  [[ -d "$cache" ]] || return
  local endpoint=()
  if [[ "$sccache_uds_supported" == true ]]; then
    endpoint+=("SCCACHE_SERVER_UDS=$sccache_runtime/$lane.sock")
  fi
  env "${endpoint[@]}" SCCACHE_DIR="$cache" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
}

stop_sccache() {
  stop_sccache_lane sccache-server
  stop_sccache_lane sccache-client
}

cleanup() {
  stop_sccache
  rm -rf -- "$sccache_runtime"
  release_benchmark_lock
}
trap cleanup EXIT

source_cargo_home="${CARGO_HOME:-${HOME:?HOME is required}/.cargo}"
source_cargo_home="$(cd "$source_cargo_home" && pwd -P)"

create_cargo_home() {
  local home="$1"
  [[ -d "$home" ]] && return
  mkdir -p "$home"
  local owned
  for owned in registry git; do
    [[ -d "$source_cargo_home/$owned" ]] || continue
    if ! ln -s "$source_cargo_home/$owned" "$home/$owned" 2>/dev/null; then
      cp -R "$source_cargo_home/$owned" "$home/$owned"
    fi
  done
}

lanes=(
  cargo-l0
  cargo-empty
  transparent-l0
  cache-off
  transparent-cold
  transparent-warm
  sccache-server
  sccache-client
  transparent-incremental
)
workloads=(check build test)
installed_lanes=(transparent-l0 cache-off transparent-cold transparent-warm transparent-incremental)

mkdir -p "$state/homes" "$state/fixtures" "$state/caches" "$results/raw"
for lane in "${lanes[@]}"; do
  create_cargo_home "$state/homes/$lane/cargo"
done

setup_lane() {
  local lane="$1"
  local home="$state/homes/$lane/cargo"
  local cache="$state/caches/$lane"
  local setup_root="$state/fixtures/check-$lane"
  mkdir -p "$cache"
  (
    cd "$setup_root"
    env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$home" \
      "$binary" rail cache setup --local-dir "$cache" --max-size 10GiB >/dev/null
  )
}

shared_git="$state/fixture-git-source"
for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    root="$state/fixtures/$workload-$lane"
    if [[ ! -f "$root/Cargo.toml" ]]; then
      "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$root" "$shared_git" >/dev/null
    fi
  done
done
compiler_mode_root="$state/fixtures/compiler-modes-transparent-warm"
if [[ ! -f "$compiler_mode_root/Cargo.toml" ]]; then
  "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$compiler_mode_root" "$shared_git" >/dev/null
fi
(
  cd "$compiler_mode_root"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$state/homes/transparent-warm/cargo" \
    cargo fetch --locked --quiet
)
for lane in "${installed_lanes[@]}"; do
  setup_lane "$lane"
done

run_compiler_mode_inventory() {
  local directory="$results/coverage/compiler-modes"
  local report="$directory/coverage.json"
  if [[ -f "$report" ]]; then
    jq -e '.schema_version == 1 and .passed' "$report" >/dev/null
    return
  fi
  local events="$directory/events"
  local stdout="$directory/stdout"
  local stderr="$directory/stderr"
  local cargo_home="$state/homes/transparent-warm/cargo"
  local rustdoc_program
  rustdoc_program="$(command -v rustdoc)"
  rm -rf -- "$directory" "$compiler_mode_root/target"
  mkdir -p "$events"
  chmod 700 "$directory" "$events"
  : >"$stdout"
  : >"$stderr"
  local environment=(
    -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER
    -u RUSTC_WORKSPACE_WRAPPER -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
    -u CARGO_TARGET_DIR -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS
    "CARGO_HOME=$cargo_home" CARGO_TERM_COLOR=never CARGO_NET_OFFLINE=true
    CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
    CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1
    "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$events"
  )
  (
    cd "$compiler_mode_root"
    env "${environment[@]}" \
      cargo clippy --workspace --all-features --all-targets --locked --offline \
        --message-format=json-render-diagnostics -vv >>"$stdout" 2>>"$stderr"
    env "${environment[@]}" RUSTDOC="$binary" CARGO_RAIL_RUSTDOC_WRAPPER=1 \
      "CARGO_RAIL_INNER_RUSTDOC=$rustdoc_program" \
      cargo doc --workspace --all-features --no-deps --locked --offline \
        --message-format=json-render-diagnostics -vv >>"$stdout" 2>>"$stderr"
    env "${environment[@]}" RUSTDOC="$binary" CARGO_RAIL_RUSTDOC_WRAPPER=1 \
      "CARGO_RAIL_INNER_RUSTDOC=$rustdoc_program" \
      cargo test --workspace --all-features --doc --locked --offline \
        --message-format=json-render-diagnostics -vv >>"$stdout" 2>>"$stderr"
  )
  python3 "$coverage" compiler-modes \
    --cargo-rail-events "$events" \
    --cargo-rail-verbose "$stderr" \
    --cargo-rail-root "$compiler_mode_root" \
    --output "$report"
}
for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    (
      cd "$state/fixtures/$workload-$lane"
      env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
        -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$state/homes/$lane/cargo" \
        cargo fetch --locked --quiet
    )
  done
done

command_args() {
  local workload="$1"
  local lane="$2"
  local corpus="${3:-performance}"
  CARGO_COMMAND=(cargo "$workload")
  [[ "$workload" == build ]] && CARGO_COMMAND+=(--release)
  [[ "$workload" == test ]] && CARGO_COMMAND+=(--no-run --all-targets)
  if [[ "$lane" == transparent-incremental ]]; then
    CARGO_COMMAND+=(--package fixture-types --no-default-features)
  else
    CARGO_COMMAND+=(--workspace --all-features)
    if [[ "$corpus" == performance ]]; then
      CARGO_COMMAND+=(--exclude fixture-static)
    fi
  fi
  CARGO_COMMAND+=(--locked --offline --message-format=json-render-diagnostics)
}

common_measure_args() {
  local root="$1"
  local home="$2"
  local stdout="$3"
  local stderr="$4"
  local output="$5"
  MEASURE_ARGS=(
    "$measure" --cwd "$root" --stdout "$stdout" --stderr "$stderr" --output "$output"
    --unset RUSTC_WRAPPER --unset CARGO_BUILD_RUSTC_WRAPPER
    --unset RUSTC_WORKSPACE_WRAPPER --unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
    --unset CARGO_TARGET_DIR --unset RUSTFLAGS --unset CARGO_ENCODED_RUSTFLAGS
    --env "CARGO_HOME=$home" --env CARGO_TERM_COLOR=never --env CARGO_NET_OFFLINE=true
  )
}

run_lane_command() {
  local workload="$1"
  local lane="$2"
  local stdout="$3"
  local stderr="$4"
  local timing="$5"
  local root="$state/fixtures/$workload-$lane"
  local home="$state/homes/$lane/cargo"
  command_args "$workload" "$lane" performance
  common_measure_args "$root" "$home" "$stdout" "$stderr" "$timing"
  MEASURE_ARGS+=(--env CARGO_INCREMENTAL=0)
  case "$lane" in
    cache-off) MEASURE_ARGS+=(--env CARGO_RAIL_CACHE=off) ;;
    transparent-incremental)
      MEASURE_ARGS+=(
        --env CARGO_INCREMENTAL=1
        --env CARGO_PROFILE_DEV_INCREMENTAL=true
        --env CARGO_PROFILE_RELEASE_INCREMENTAL=true
      )
      ;;
    sccache-server | sccache-client)
      local cache="$state/homes/$lane/cache"
      mkdir -p "$cache"
      MEASURE_ARGS+=(--env "RUSTC_WRAPPER=$sccache_bin" --env "SCCACHE_DIR=$cache")
      if [[ "$sccache_uds_supported" == true ]]; then
        MEASURE_ARGS+=(--env "SCCACHE_SERVER_UDS=$sccache_runtime/$lane.sock")
      fi
      if [[ "$lane" == sccache-client ]]; then
        MEASURE_ARGS+=(--env SCCACHE_CLIENT_SIDE=1)
      else
        MEASURE_ARGS+=(--unset SCCACHE_CLIENT_SIDE)
      fi
      ;;
  esac
  python3 "${MEASURE_ARGS[@]}" -- "${CARGO_COMMAND[@]}"
}

manifest_outputs() {
  local root="$1"
  local cargo_output="$2"
  local lane="$3"
  local output="$4"
  local target="$root/target"
  local -a rust_output_names=(-name '*.d' -o -name '*.rmeta')
  if [[ "$lane" != transparent-incremental ]]; then
    rust_output_names+=(-o -name '*.rlib')
  fi
  : >"$output"
  [[ -d "$target" ]] || return
  while IFS= read -r path; do
    [[ "$path" == "$target/"* && -f "$path" && ! -L "$path" ]] || {
      echo "native-cache Cargo output is not one target-contained regular file: $path" >&2
      return 1
    }
    local mode
    if mode="$(stat -f '%Lp' "$path" 2>/dev/null)"; then
      :
    else
      mode="$(stat -c '%a' "$path")"
    fi
    printf '%s  %s  %s\n' "$(sha256_file "$path")" "$mode" "${path#"$root"/}" >>"$output"
  done < <(
    {
      find "$target" -type f ! -path '*/incremental/*' \( "${rust_output_names[@]}" \) -print
      if [[ "$lane" == transparent-warm ]]; then
        jq -Rr 'fromjson? | select(.reason == "compiler-artifact") | (.filenames[]?), (.executable // empty)' \
          "$cargo_output"
      fi
    } | LC_ALL=C sort -u
  )
}

status_usage() {
  local workload="$1"
  local lane="$2"
  local output="$3"
  local root="$state/fixtures/$workload-$lane"
  local home="$state/homes/$lane/cargo"
  (
    cd "$root"
    env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$home" \
      "$binary" rail cache status --scope local -f json >"$output"
  )
}

seed_lane() {
  local workload="$1"
  local lane="$2"
  local root="$state/fixtures/$workload-$lane"
  local reference="$state/$workload-$lane.reference"
  [[ -f "$reference" ]] && return
  local seed="$state/$workload-$lane.seed"
  mkdir -p "$seed"
  run_lane_command "$workload" "$lane" "$seed/stdout" "$seed/stderr" "$seed/timing.json"
  manifest_outputs "$root" "$seed/stdout" "$lane" "$reference"
  [[ -s "$reference" ]] || {
    echo "native-cache seed produced no authoritative outputs: $workload/$lane" >&2
    exit 1
  }
}

for workload in "${workloads[@]}"; do
  for lane in cargo-l0 transparent-l0 transparent-warm sccache-server sccache-client; do
    seed_lane "$workload" "$lane"
  done
done

identity_directory="$results"
if [[ "$operation" == resume ]]; then
  identity_directory="$state/resume-identity"
  mkdir -p "$identity_directory"
fi
harness_manifest="$identity_directory/harness-sha256.txt"
: >"$harness_manifest"
for harness in \
  scripts/bench/measure-command.py \
  scripts/bench/native-cache-coverage.py \
  scripts/bench/native-cache-report.sh \
  scripts/bench/native-cache.sh \
  scripts/bench/rustc-timing-wrapper.sh \
  scripts/fixtures/materialize-native-cache.sh; do
  printf '%s  %s\n' "$(sha256_file "$repo_root/$harness")" "$harness" >>"$harness_manifest"
done
harness_sha256="$(sha256_file "$harness_manifest")"
untracked_manifest="$identity_directory/untracked-sha256.txt"
: >"$untracked_manifest"
while IFS= read -r -d '' untracked; do
  if [[ -f "$repo_root/$untracked" ]]; then
    printf '%s  %s\n' "$(sha256_file "$repo_root/$untracked")" "$untracked" >>"$untracked_manifest"
  else
    printf 'non-regular  %s\n' "$untracked" >>"$untracked_manifest"
  fi
done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
untracked_sha256="$(sha256_file "$untracked_manifest")"
worktree_status="$identity_directory/worktree-status.txt"
git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$worktree_status"
worktree_status_sha256="$(sha256_file "$worktree_status")"
worktree_diff="$identity_directory/worktree.diff"
capture_worktree_patch "$worktree_diff"
worktree_diff_sha256="$(sha256_file "$worktree_diff")"

if [[ "$operation" == resume ]]; then
  [[ "$(jq -r '.repository_commit' "$results/environment.json")" == "$(git -C "$repo_root" rev-parse HEAD)" ]] || {
    echo "native-cache resume repository commit changed; start a new result" >&2
    exit 2
  }
  if ! jq -e \
    --arg diff "$worktree_diff_sha256" \
    --arg status "$worktree_status_sha256" \
    --arg untracked "$untracked_sha256" \
    --arg harness "$harness_sha256" \
    --arg binary "$(sha256_file "$binary")" '
      .worktree_diff_sha256 == $diff
      and .worktree_status_sha256 == $status
      and .untracked_sha256 == $untracked
      and .benchmark_harness_sha256 == $harness
      and .release_binary_sha256 == $binary
    ' "$results/environment.json" >/dev/null; then
    echo "native-cache resume worktree changed; start a new result" >&2
    exit 2
  fi
else
  jq -n \
    --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
    --arg diff_sha256 "$worktree_diff_sha256" \
    --arg status_sha256 "$worktree_status_sha256" \
    --arg untracked_sha256 "$untracked_sha256" \
    --arg binary_sha256 "$(sha256_file "$binary")" \
    --arg harness_sha256 "$harness_sha256" \
    --arg rustc "$(rustc -vV)" \
    --arg cargo "$(cargo -Vv)" \
    --arg host "$(uname -a)" \
    --arg sccache "$sccache_version" \
    '{
      schema_version: 11,
      generated_at: $generated_at,
      repository_commit: $commit,
      worktree_diff_sha256: $diff_sha256,
      worktree_status_sha256: $status_sha256,
      untracked_sha256: $untracked_sha256,
      release_binary_sha256: $binary_sha256,
      benchmark_harness_sha256: $harness_sha256,
      rustc: $rustc,
      cargo: $cargo,
      host: $host,
      sccache: $sccache,
      authority: "isolated Cargo homes and receipt-selected private CAS roots",
      remote_activation: false
    }' >"$results/environment.json"
  jq -n \
    --arg evidence_kind "$evidence_kind" \
    --argjson runs "$runs" \
    --argjson lanes "$(printf '%s\n' "${lanes[@]}" | jq -Rsc 'split("\n")[:-1]')" \
    --argjson workloads "$(printf '%s\n' "${workloads[@]}" | jq -Rsc 'split("\n")[:-1]')" \
    '{
      schema_version: 14,
      evidence_kind: $evidence_kind,
      required_accepted_samples: $runs,
      workloads: $workloads,
      lanes: $lanes,
      interleaving: "deterministic rotation by workload and round",
      performance_exclusions: {
        "fixture-static": "pinned sccache omits the requested metadata output on a cache hit"
      },
      qualification: {
        rust_action_coverage_gate: "strict_safe_action_superset",
        minimum_samples: 20,
        l0_and_disabled_p95_overhead_max_percent: 3,
        l0_and_disabled_p95_overhead_max_seconds: 0.05,
        empty_target_l1_vs_sccache_p50_and_p95_min_percent: 10,
        empty_target_l1_vs_sccache_p50_and_p95_target_percent: 15
      }
    }' >"$results/run.json"
fi

run_coverage_command() {
  local workload="$1"
  local lane="$2"
  local directory="$3"
  local retain_evidence="${4:-true}"
  local root="$state/fixtures/$workload-$lane"
  local home="$state/homes/$lane/cargo"
  command_args "$workload" "$lane" coverage
  CARGO_COMMAND+=(--verbose --verbose)
  common_measure_args "$root" "$home" "$directory/stdout" "$directory/stderr" "$directory/timing.json"
  MEASURE_ARGS+=(--env CARGO_INCREMENTAL=0 --env CARGO_BUILD_JOBS=1 --env CC_ENABLE_DEBUG_OUTPUT=1)
  case "$lane" in
    transparent-warm)
      if [[ "$retain_evidence" == true ]]; then
        MEASURE_ARGS+=(
          --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1
          --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$directory/events"
        )
      else
        MEASURE_ARGS+=(--unset CARGO_RAIL_CACHE --unset CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY)
      fi
      ;;
    sccache-server)
      local cache="$state/homes/$lane/cache"
      MEASURE_ARGS+=(--env "RUSTC_WRAPPER=$sccache_bin" --env "SCCACHE_DIR=$cache" --unset SCCACHE_CLIENT_SIDE)
      if [[ "$retain_evidence" == true ]]; then
        MEASURE_ARGS+=(--env "SCCACHE_ERROR_LOG=$directory/sccache.log" --env SCCACHE_LOG=trace)
      else
        MEASURE_ARGS+=(--unset SCCACHE_ERROR_LOG --unset SCCACHE_LOG)
      fi
      if [[ "$sccache_uds_supported" == true ]]; then
        MEASURE_ARGS+=(--env "SCCACHE_SERVER_UDS=$sccache_runtime/$lane.sock")
      fi
      ;;
    *)
      echo "unsupported native-cache coverage lane: $lane" >&2
      return 2
      ;;
  esac
  python3 "${MEASURE_ARGS[@]}" -- "${CARGO_COMMAND[@]}"
}

run_cold_cost_ledger() {
  local workload="$1"
  local directory="$2"
  local root="$state/fixtures/$workload-transparent-cold"
  local home="$state/homes/transparent-cold/cargo"
  [[ -f "$directory/.complete" ]] && return
  rm -rf -- "$directory" "$root/target"
  mkdir -p "$directory"
  command_args "$workload" transparent-cold coverage
  (
    cd "$root"
    env -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
      CARGO_HOME="$home" CARGO_NET_OFFLINE=true CARGO_TERM_COLOR=never CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
      CARGO_RAIL_CACHE=off CARGO_RAIL_UNIT_TIMINGS="$directory" CARGO_RAIL_BENCH_WORKSPACE="$root" \
      RUSTC_WRAPPER="$timing_wrapper" "${CARGO_COMMAND[@]}" >"$directory/cargo.stdout" 2>"$directory/cargo.stderr"
  )
  [[ -n "$(find "$directory" -maxdepth 1 -type f -name 'unit-*.tsv' -print -quit)" ]] || {
    echo "native-cache cold cost ledger produced no compiler records: $workload" >&2
    return 1
  }
  : >"$directory/.complete"
}

coverage_failures=0
for workload in "${workloads[@]}"; do
  directory="$results/coverage/$workload"
  report="$directory/coverage.json"
  if [[ -f "$report" ]]; then
    jq -e '.schema_version == 6 and .coverage_gate.passed' "$report" >/dev/null || coverage_failures=$((coverage_failures + 1))
    continue
  fi
  rm -rf -- "$directory"
  mkdir -p "$directory/cargo-rail/events" "$directory/sccache"
  chmod 700 "$directory" "$directory/cargo-rail" "$directory/cargo-rail/events" "$directory/sccache"
  run_cold_cost_ledger "$workload" "$directory/cold-costs"
  rm -rf -- \
    "$state/fixtures/$workload-transparent-warm/target" \
    "$state/fixtures/$workload-sccache-server/target"
  mkdir -p "$directory/prime/cargo-rail" "$directory/prime/sccache"
  run_coverage_command "$workload" transparent-warm "$directory/prime/cargo-rail" false
  stop_sccache_lane sccache-server
  run_coverage_command "$workload" sccache-server "$directory/prime/sccache" false
  stop_sccache_lane sccache-server
  rm -rf -- \
    "$state/fixtures/$workload-transparent-warm/target" \
    "$state/fixtures/$workload-sccache-server/target"
  run_coverage_command "$workload" transparent-warm "$directory/cargo-rail"
  stop_sccache_lane sccache-server
  run_coverage_command "$workload" sccache-server "$directory/sccache"
  stop_sccache_lane sccache-server
  if ! python3 "$coverage" report \
    --cargo-rail-events "$directory/cargo-rail/events" \
    --cargo-rail-messages "$directory/cargo-rail/stdout" \
    --cargo-rail-verbose "$directory/cargo-rail/stderr" \
    --cargo-rail-root "$state/fixtures/$workload-transparent-warm" \
    --sccache-log "$directory/sccache/sccache.log" \
    --sccache-root "$state/fixtures/$workload-sccache-server" \
    --cold-timings "$directory/cold-costs" \
    --output "$report"; then
    coverage_failures=$((coverage_failures + 1))
  fi
done
if ! run_compiler_mode_inventory; then
  coverage_failures=$((coverage_failures + 1))
fi
if [[ "$coverage_failures" -ne 0 ]]; then
  echo "native-cache timing blocked: verified Rust action coverage does not pass the four-dimensional gate" >&2
  exit 1
fi

usage_delta_json() {
  local before="$1"
  local after="$2"
  jq -n --slurpfile before "$before" --slurpfile after "$after" '
    ($before[0].status.installation.usage) as $b
    | ($after[0].status.installation.usage) as $a
    | {
        hits: ($a.hits - $b.hits),
        misses: ($a.misses - $b.misses),
        bypasses: ($a.bypasses - $b.bypasses),
        failures: ($a.failures - $b.failures),
        recorded_events: ($a.recorded_events - $b.recorded_events),
        early_bypasses: $a.early_bypasses
      }'
}

sample_lane() {
  local workload="$1"
  local lane="$2"
  local round="$3"
  local directory="$results/raw/$workload/round-$round/$lane"
  local sample="$directory/sample.json"
  [[ -f "$sample" ]] && return
  mkdir -p "$directory"
  local root="$state/fixtures/$workload-$lane"
  case "$lane" in
    cargo-l0 | transparent-l0) ;;
    transparent-cold)
      rm -rf -- "$root/target" "$state/caches/$lane"
      setup_lane "$lane"
      ;;
    *) rm -rf -- "$root/target" ;;
  esac

  local before="$directory/status-before.json"
  local after="$directory/status-after.json"
  local usage='null'
  case "$lane" in
    transparent-* | cache-off) status_usage "$workload" "$lane" "$before" ;;
  esac

  local command_ok=true
  if ! run_lane_command "$workload" "$lane" "$directory/stdout" "$directory/stderr" "$directory/timing.json"; then
    command_ok=false
  fi
  case "$lane" in
    transparent-* | cache-off)
      status_usage "$workload" "$lane" "$after"
      usage="$(usage_delta_json "$before" "$after")"
      ;;
  esac

  manifest_outputs "$root" "$directory/stdout" "$lane" "$directory/outputs.sha256"
  local reference="$state/$workload-$lane.reference"
  if [[ ! -f "$reference" ]]; then
    cp "$directory/outputs.sha256" "$reference"
  fi
  local outputs_identical=false
  cmp -s "$reference" "$directory/outputs.sha256" && outputs_identical=true
  local cache_outcome_valid=true
  case "$lane" in
    transparent-warm)
      jq -e '.hits > 0 and .failures == 0' <<<"$usage" >/dev/null || cache_outcome_valid=false
      ;;
    transparent-cold)
      jq -e '.hits == 0 and .misses > 0 and .failures == 0' <<<"$usage" >/dev/null || cache_outcome_valid=false
      ;;
    transparent-l0 | cache-off)
      jq -e '.hits == 0 and .misses == 0 and .failures == 0' <<<"$usage" >/dev/null || cache_outcome_valid=false
      ;;
    transparent-incremental)
      jq -e '.hits == 0 and .misses == 0 and .failures == 0 and .recorded_events == 0' <<<"$usage" >/dev/null \
        || cache_outcome_valid=false
      ;;
  esac
  local accepted=false
  if [[ "$command_ok" == true && "$outputs_identical" == true && "$cache_outcome_valid" == true ]]; then
    accepted=true
  fi
  jq -n \
    --arg sample_id "$workload-$round-$lane" \
    --arg workload "$workload" \
    --arg lane "$lane" \
    --argjson round "$round" \
    --argjson command_ok "$command_ok" \
    --argjson outputs_identical "$outputs_identical" \
    --argjson cache_outcome_valid "$cache_outcome_valid" \
    --argjson accepted "$accepted" \
    --argjson usage "$usage" \
    --slurpfile timing "$directory/timing.json" \
    '{
      schema_version: 11,
      sample_id: $sample_id,
      workload: $workload,
      lane: $lane,
      round: $round,
      accepted: $accepted,
      command_ok: $command_ok,
      outputs_identical_to_lane_authority: $outputs_identical,
      cache_outcome_valid: $cache_outcome_valid,
      usage: $usage,
      measurement: $timing[0]
    }' >"$sample"
  [[ "$accepted" == true ]] || {
    echo "native-cache sample rejected: $workload/$round/$lane" >&2
    return 1
  }
}

for ((round = 1; round <= runs; round++)); do
  for workload_index in "${!workloads[@]}"; do
    workload="${workloads[$workload_index]}"
    offset=$(((round + workload_index - 1) % ${#lanes[@]}))
    for ((position = 0; position < ${#lanes[@]}; position++)); do
      lane="${lanes[$(((offset + position) % ${#lanes[@]}))]}"
      sample_lane "$workload" "$lane" "$round"
    done
  done
done

"$reporter" validate "$results"
echo "native-cache benchmark result: $results"
