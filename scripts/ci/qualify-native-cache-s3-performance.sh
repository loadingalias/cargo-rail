#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
measure="$repo_root/scripts/bench/measure-command.py"
coverage_reporter="$repo_root/scripts/bench/native-cache-coverage.py"

usage() {
  echo "usage: $0 <20-30 rounds> <run-id> <cargo-rail-s3-url> <sccache-bucket> <sccache-region> <sccache-prefix>" >&2
  echo "       CARGO_RAIL_PERFORMANCE_SMOKE=1 permits exactly one non-qualifying run" >&2
  exit 2
}

runs="${1:-}"
run_id="${2:-}"
remote_url="${3:-}"
sccache_bucket="${4:-}"
sccache_region="${5:-}"
sccache_prefix="${6:-}"
smoke="${CARGO_RAIL_PERFORMANCE_SMOKE:-0}"
case "$smoke" in
  0) [[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 20 && "$runs" -le 30 ]] || usage ;;
  1) [[ "$runs" == 1 ]] || usage ;;
  *) usage ;;
esac
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
[[ "$remote_url" == "s3://$sccache_bucket/"* ]] || usage
[[ "$sccache_region" =~ ^[a-z0-9-]+$ ]] || usage
[[ "$sccache_prefix" =~ ^[A-Za-z0-9._/-]+$ && "$sccache_prefix" != /* && "$sccache_prefix" != */ ]] || usage
[[ "$#" -eq 6 ]] || usage

for tool in cargo git jq python3 sccache; do
  command -v "$tool" >/dev/null || {
    echo "official-S3 performance qualification requires $tool" >&2
    exit 2
  }
done

benchmark_lock="$repo_root/target/benchmarks/.native-cache-s3-performance.lock"
mkdir -p "$(dirname "$benchmark_lock")"
if ! mkdir "$benchmark_lock" 2>/dev/null; then
  owner="$(cat "$benchmark_lock/pid" 2>/dev/null || true)"
  echo "official-S3 performance qualification is already running${owner:+ as PID $owner}: $benchmark_lock" >&2
  exit 2
fi
printf '%s\n' "$$" >"$benchmark_lock/pid"

release_benchmark_lock() {
  [[ "$(cat "$benchmark_lock/pid" 2>/dev/null || true)" == "$$" ]] || return
  rm -rf -- "$benchmark_lock"
}
trap release_benchmark_lock EXIT

expected_sccache="$(sed -n 's/^readonly SCCACHE_VERSION=//p' "$repo_root/scripts/ci/install-tools.sh")"
sccache_bin="${SCCACHE_BIN:-$(command -v sccache)}"
sccache_version="$($sccache_bin --version)"
[[ "$sccache_version" == "sccache $expected_sccache" ]] || {
  echo "official-S3 performance qualification requires pinned sccache $expected_sccache; found $sccache_version" >&2
  exit 2
}

binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
if [[ -z "${CARGO_RAIL_BIN+x}" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
fi
[[ -x "$binary" ]] || {
  echo "official-S3 performance qualification binary is not executable: $binary" >&2
  exit 2
}
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"

normalized="$($binary rail cache normalize "$remote_url" --mode read-write -f json)"
jq -e '.remote.provider == "aws-s3" and .remote.activation == "direct_transport_selected"' <<<"$normalized" \
  >/dev/null || {
  echo "official-S3 performance qualification requires a normalized official AWS authority" >&2
  exit 2
}
normalized_url="$(jq -r '.normalized_url' <<<"$normalized")"
location="${normalized_url%%\?*}"
authority="${location#s3://}"
normalized_bucket="${authority%%/*}"
cargo_rail_prefix="${authority#*/}"
expected_cargo_rail_prefix="cache/cargo-rail/s3/v3/task9/$run_id"
expected_sccache_prefix="cache/cargo-rail/sccache/v3/aws/task9/$run_id"
[[ "$normalized_bucket" == "$sccache_bucket" && "$cargo_rail_prefix" == "$expected_cargo_rail_prefix" ]] || {
  echo "official-S3 performance qualification requires exact Cargo-Rail prefix $expected_cargo_rail_prefix" >&2
  exit 2
}
[[ "$sccache_prefix" == "$expected_sccache_prefix" ]] || {
  echo "official-S3 performance qualification requires exact sccache prefix $expected_sccache_prefix" >&2
  exit 2
}

results="$repo_root/benchmark_results/native-cache/$run_id"
state="$repo_root/benchmark_results/native-cache-s3-performance-state/$run_id"
[[ ! -e "$results" ]] || {
  echo "official-S3 performance result already exists: $results" >&2
  exit 2
}
[[ ! -e "$state" ]] || {
  echo "official-S3 performance state already exists: $state" >&2
  exit 2
}
mkdir -p "$results/raw" "$state/cargo-homes" "$state/caches" "$state/fixtures" "$state/sccache"
chmod 700 "$results" "$results/raw" "$state" "$state/cargo-homes" "$state/caches" "$state/fixtures" \
  "$state/sccache"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sha256_text() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

capture_worktree_patch() {
  local output="$1"
  git -C "$repo_root" diff --binary HEAD -- >"$output"
  while IFS= read -r -d '' untracked; do
    [[ -f "$repo_root/$untracked" || -L "$repo_root/$untracked" ]] || {
      echo "official-S3 performance evidence cannot capture non-file untracked input: $untracked" >&2
      return 1
    }
    local status=0
    git -C "$repo_root" diff --binary --no-index -- /dev/null "$untracked" >>"$output" || status=$?
    [[ "$status" -eq 1 ]] || {
      echo "official-S3 performance evidence could not capture untracked input: $untracked" >&2
      return 1
    }
  done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
}

workloads=(check build test)
lanes=(cargo-rail-s3 sccache-s3)
source_cargo_home="${CARGO_HOME:-${HOME:?HOME is required}/.cargo}"
source_cargo_home="$(cd "$source_cargo_home" && pwd -P)"
# Fixture registry dependencies are exact entries in the root lockfile. Seed them before the
# materializer enforces offline lockfile generation; prebuilt binaries do not warm Cargo's registry.
env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
  -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$source_cargo_home" \
  cargo fetch --manifest-path "$repo_root/Cargo.toml" --locked --quiet
shared_git="$state/fixture-git-source"

for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    root="$state/fixtures/$workload-$lane"
    cargo_home="$state/cargo-homes/$workload-$lane"
    mkdir -p "$cargo_home"
    chmod 700 "$cargo_home"
    if [[ -d "$source_cargo_home/registry" ]]; then
      ln -s "$source_cargo_home/registry" "$cargo_home/registry"
    fi
    "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$root" "$shared_git" >/dev/null
    (
      cd "$root"
      env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
        -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$cargo_home" cargo fetch --locked --quiet
    )
  done
done

manifest_outputs() {
  local root="$1"
  local cargo_output="$2"
  local output="$3"
  local target="$root/target"
  : >"$output"
  [[ -d "$target" ]] || return
  while IFS= read -r path; do
    [[ "$path" == "$target/"* && -f "$path" && ! -L "$path" ]] || {
      echo "official-S3 performance output is not one target-contained regular file: $path" >&2
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
      find "$target" -type f ! -path '*/incremental/*' \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) \
        -print
      jq -Rr 'fromjson? | select(.reason == "compiler-artifact") | (.filenames[]?), (.executable // empty)' \
        "$cargo_output"
    } | LC_ALL=C sort -u
  )
}

event_summary() {
  local directory="$1"
  local -a events=()
  mapfile -d '' -t events < <(find "$directory" -type f -name 'event-*.json' -print0 | sort -z)
  jq -s '
    {
      events: length,
      hits: ([.[] | select(.status == "hit")] | length),
      misses: ([.[] | select(.status == "miss")] | length),
      bypasses: ([.[] | select(.status == "bypassed" or .status == "disabled")] | length),
      remote_hits: ([.[] | select(.status == "hit" and .reason == "verified_remote_result")] | length),
      remote_publications: ([.[] | select(.reason | contains("remote_published"))] | length),
      remote_request_attempts: ([.[].remote_request_attempts] | add // 0),
      remote_coordinator_requests: ([.[].remote_coordinator_requests] | add // 0),
      remote_payload_bytes_read: ([.[].remote_payload_bytes_read] | add // 0),
      remote_payload_bytes_written: ([.[].remote_payload_bytes_written] | add // 0),
      remote_service_elapsed_ns: ([.[].remote_service_elapsed_ns] | add // 0),
      timing: {
        total: {
          count: ([.[].timing.total.count] | add // 0),
          elapsed_ns: ([.[].timing.total.elapsed_ns] | add // 0)
        },
        store_connect: {
          count: ([.[].timing.store_connect.count] | add // 0),
          elapsed_ns: ([.[].timing.store_connect.elapsed_ns] | add // 0)
        },
        lookup: {
          count: ([.[].timing.lookup.count] | add // 0),
          elapsed_ns: ([.[].timing.lookup.elapsed_ns] | add // 0)
        },
        decode: {
          count: ([.[].timing.decode.count] | add // 0),
          elapsed_ns: ([.[].timing.decode.elapsed_ns] | add // 0)
        },
        validation: {
          count: ([.[].timing.validation.count] | add // 0),
          elapsed_ns: ([.[].timing.validation.elapsed_ns] | add // 0)
        },
        l1_admission: {
          count: ([.[].timing.l1_admission.count] | add // 0),
          elapsed_ns: ([.[].timing.l1_admission.elapsed_ns] | add // 0)
        },
        output_restore: {
          count: ([.[].timing.output_restore.count] | add // 0),
          elapsed_ns: ([.[].timing.output_restore.elapsed_ns] | add // 0)
        }
      },
      durability: {
        l1_file_sync: {
          count: ([.[].durability.l1_file_sync.count] | add // 0),
          elapsed_ns: ([.[].durability.l1_file_sync.elapsed_ns] | add // 0)
        },
        l1_directory_sync: {
          count: ([.[].durability.l1_directory_sync.count] | add // 0),
          elapsed_ns: ([.[].durability.l1_directory_sync.elapsed_ns] | add // 0)
        },
        output_file_sync: {
          count: ([.[].durability.output_file_sync.count] | add // 0),
          elapsed_ns: ([.[].durability.output_file_sync.elapsed_ns] | add // 0)
        },
        output_directory_sync: {
          count: ([.[].durability.output_directory_sync.count] | add // 0),
          elapsed_ns: ([.[].durability.output_directory_sync.elapsed_ns] | add // 0)
        },
        cas_lock_wait: {
          count: ([.[].durability.cas_lock_wait.count] | add // 0),
          elapsed_ns: ([.[].durability.cas_lock_wait.elapsed_ns] | add // 0)
        },
        cas_commit: {
          count: ([.[].durability.cas_commit.count] | add // 0),
          elapsed_ns: ([.[].durability.cas_commit.elapsed_ns] | add // 0)
        },
        restore_lock_wait: {
          count: ([.[].durability.restore_lock_wait.count] | add // 0),
          elapsed_ns: ([.[].durability.restore_lock_wait.elapsed_ns] | add // 0)
        },
        restore_transaction: {
          count: ([.[].durability.restore_transaction.count] | add // 0),
          elapsed_ns: ([.[].durability.restore_transaction.elapsed_ns] | add // 0)
        }
      },
      remote_errors: ([.[].remote_error? | select(. != null)] | unique)
    }' "${events[@]}"
}

command_args() {
  local workload="$1"
  cargo_command=(cargo "$workload")
  [[ "$workload" == build ]] && cargo_command+=(--release)
  [[ "$workload" == test ]] && cargo_command+=(--no-run --all-targets)
  cargo_command+=(
    --workspace --all-features --exclude fixture-static
    --locked --offline --message-format=json-render-diagnostics
  )
}

measure_args() {
  local root="$1"
  local cargo_home="$2"
  local directory="$3"
  measurement=(
    "$measure"
    --cwd "$root"
    --stdout "$directory/stdout"
    --stderr "$directory/stderr"
    --output "$directory/timing.json"
    --unset RUSTC_WRAPPER
    --unset CARGO_BUILD_RUSTC_WRAPPER
    --unset RUSTC_WORKSPACE_WRAPPER
    --unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
    --unset CARGO_TARGET_DIR
    --unset RUSTFLAGS
    --unset CARGO_ENCODED_RUSTFLAGS
    --unset AWS_ENDPOINT_URL
    --unset AWS_ENDPOINT_URL_S3
    --env "CARGO_HOME=$cargo_home"
    --env CARGO_TERM_COLOR=never
    --env CARGO_NET_OFFLINE=true
    --env CARGO_INCREMENTAL=0
  )
}

setup_cargo_rail() {
  local workload="$1"
  local cache="$2"
  local mode="$3"
  local root="$state/fixtures/$workload-cargo-rail-s3"
  local cargo_home="$state/cargo-homes/$workload-cargo-rail-s3"
  rm -rf -- "$cache"
  mkdir -p "$cache"
  (
    cd "$root"
    env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$cargo_home" \
      "$binary" rail cache setup --local-dir "$cache" --max-size 10GiB \
        --remote "$remote_url" --remote-mode "$mode" >/dev/null
  )
}

run_cargo_rail() {
  local workload="$1"
  local directory="$2"
  local mode="$3"
  local cache="$4"
  local root="$state/fixtures/$workload-cargo-rail-s3"
  local cargo_home="$state/cargo-homes/$workload-cargo-rail-s3"
  rm -rf -- "$root/target"
  setup_cargo_rail "$workload" "$cache" "$mode"
  mkdir -p "$directory/events"
  chmod 700 "$directory" "$directory/events"
  command_args "$workload"
  measure_args "$root" "$cargo_home" "$directory"
  measurement+=(
    --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1
    --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$directory/events"
  )
  python3 "${measurement[@]}" -- "${cargo_command[@]}"
  manifest_outputs "$root" "$directory/stdout" "$directory/outputs.sha256"
  event_summary "$directory/events" >"$directory/cache.json"
}

sccache_environment() {
  local directory="$1"
  local mode="$2"
  local socket
  socket="${TMPDIR:-/tmp}/cargo-rail-sccache-$(printf '%s' "$directory" | sha256_text | cut -c1-16).sock"
  sccache_env=(
    "SCCACHE_BUCKET=$sccache_bucket"
    "SCCACHE_REGION=$sccache_region"
    "SCCACHE_S3_KEY_PREFIX=$sccache_prefix"
    SCCACHE_S3_USE_SSL=true
    "SCCACHE_S3_RW_MODE=$mode"
    "SCCACHE_DIR=$directory/cache"
    "SCCACHE_SERVER_UDS=$socket"
  )
}

stop_sccache() {
  local directory="$1"
  local mode="$2"
  sccache_environment "$directory" "$mode"
  env "${sccache_env[@]}" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
}

active_sccache_directory=""
active_sccache_mode=""

cleanup() {
  if [[ -n "$active_sccache_directory" ]]; then
    stop_sccache "$active_sccache_directory" "$active_sccache_mode"
  fi
  release_benchmark_lock
}
trap cleanup EXIT

run_sccache() {
  local workload="$1"
  local directory="$2"
  local mode="$3"
  local root="$state/fixtures/$workload-sccache-s3"
  local cargo_home="$state/cargo-homes/$workload-sccache-s3"
  local socket
  rm -rf -- "$root/target" "$directory"
  mkdir -p "$directory/cache"
  chmod 700 "$directory" "$directory/cache"
  sccache_environment "$directory" "$mode"
  active_sccache_directory="$directory"
  active_sccache_mode="$mode"
  socket="$(printf '%s\n' "${sccache_env[@]}" | sed -n 's/^SCCACHE_SERVER_UDS=//p')"
  rm -f -- "$socket"
  command_args "$workload"
  measure_args "$root" "$cargo_home" "$directory"
  measurement+=(
    --env "RUSTC_WRAPPER=$sccache_bin"
    --env "SCCACHE_BUCKET=$sccache_bucket"
    --env "SCCACHE_REGION=$sccache_region"
    --env "SCCACHE_S3_KEY_PREFIX=$sccache_prefix"
    --env SCCACHE_S3_USE_SSL=true
    --env "SCCACHE_S3_RW_MODE=$mode"
    --env "SCCACHE_DIR=$directory/cache"
    --env "SCCACHE_SERVER_UDS=$socket"
    --env "SCCACHE_ERROR_LOG=$directory/sccache.log"
    --env SCCACHE_LOG=trace
  )
  python3 "${measurement[@]}" -- "${cargo_command[@]}"
  env "${sccache_env[@]}" "$sccache_bin" --show-stats --stats-format json >"$directory/cache.json"
  stop_sccache "$directory" "$mode"
  active_sccache_directory=""
  active_sccache_mode=""
  manifest_outputs "$root" "$directory/stdout" "$directory/outputs.sha256"
}

for workload in "${workloads[@]}"; do
  directory="$results/seed/$workload/cargo-rail-s3"
  mkdir -p "$directory"
  run_cargo_rail "$workload" "$directory" read-write "$state/caches/seed-$workload"
  jq -e '
    .misses > 0
    and .remote_publications > 0
    and (.remote_request_attempts > 0 or .remote_coordinator_requests > 0)
    and .remote_payload_bytes_written > 0
    and (.remote_errors | length) == 0
  ' "$directory/cache.json" >/dev/null || {
    echo "official-S3 performance seed did not publish Cargo-Rail results: $workload" >&2
    exit 1
  }

  directory="$results/seed/$workload/sccache-s3"
  run_sccache "$workload" "$directory" READ_WRITE
  jq -e '
    .stats.cache_writes > 0
    and .stats.compilations > 0
    and .stats.cache_write_errors == 0
    and (.stats.cache_errors.counts | length) == 0
  ' "$directory/cache.json" >/dev/null || {
    echo "official-S3 performance seed did not publish sccache results: $workload" >&2
    exit 1
  }
done

sample_lane() {
  local workload="$1"
  local lane="$2"
  local round="$3"
  local directory="$results/raw/$workload/round-$round/$lane"
  mkdir -p "$directory"
  local outcome_ok=false
  case "$lane" in
    cargo-rail-s3)
      run_cargo_rail "$workload" "$directory" read "$state/caches/$workload-round-$round"
      jq -e '
        .hits > 0
        and .misses == 0
        and .remote_hits > 0
        and (.remote_request_attempts > 0 or .remote_coordinator_requests > 0)
        and .remote_payload_bytes_read > 0
        and .remote_payload_bytes_written == 0
        and (.remote_errors | length) == 0
      ' "$directory/cache.json" >/dev/null && outcome_ok=true
      ;;
    sccache-s3)
      run_sccache "$workload" "$directory" READ_ONLY
      jq -e '
        ([.stats.cache_hits.counts[]] | add // 0) > 0
        and ([.stats.cache_misses.counts[]] | add // 0) == 0
        and .stats.cache_read_errors == 0
        and .stats.cache_write_errors == 0
        and .stats.cache_writes == 0
        and .stats.compilations == 0
        and (.stats.cache_errors.counts | length) == 0
      ' "$directory/cache.json" >/dev/null && outcome_ok=true
      ;;
    *) return 2 ;;
  esac
  local outputs_identical=false
  cmp -s "$results/seed/$workload/$lane/outputs.sha256" "$directory/outputs.sha256" && outputs_identical=true
  local accepted=false
  [[ "$outcome_ok" == true && "$outputs_identical" == true ]] && accepted=true
  jq -n \
    --arg sample_id "$workload-$round-$lane" \
    --arg workload "$workload" \
    --arg lane "$lane" \
    --argjson round "$round" \
    --argjson outcome_ok "$outcome_ok" \
    --argjson outputs_identical "$outputs_identical" \
    --argjson accepted "$accepted" \
    --slurpfile timing "$directory/timing.json" \
    --slurpfile cache "$directory/cache.json" \
    '{
      schema_version: 1,
      sample_id: $sample_id,
      workload: $workload,
      lane: $lane,
      round: $round,
      accepted: $accepted,
      cache_outcome_valid: $outcome_ok,
      outputs_identical_to_lane_authority: $outputs_identical,
      measurement: $timing[0],
      cache: $cache[0]
    }' >"$directory/sample.json"
  [[ "$accepted" == true ]] || {
    echo "official-S3 performance sample rejected: $workload/$round/$lane" >&2
    return 1
  }
}

verify_remote_coverage() {
  local workload="$1"
  local round="$2"
  local directory="$results/raw/$workload/round-$round"
  local cargo_rail="$directory/cargo-rail-s3/cache.json"
  local seed="$results/seed/$workload/cargo-rail-s3/cache.json"
  local semantic="$directory/semantic-coverage.json"
  if ! python3 "$coverage_reporter" remote \
    --cargo-rail-events "$directory/cargo-rail-s3/events" \
    --cargo-rail-verbose "$directory/cargo-rail-s3/stderr" \
    --cargo-rail-root "$state/fixtures/$workload-cargo-rail-s3" \
    --sccache-log "$directory/sccache-s3/sccache.log" \
    --sccache-root "$state/fixtures/$workload-sccache-s3" \
    --output "$semantic"; then
    echo "official-S3 performance safe action coverage failed: $workload/round-$round" >&2
    return 1
  fi
  jq -n \
    --arg workload "$workload" \
    --argjson round "$round" \
    --slurpfile cargo_rail "$cargo_rail" \
    --slurpfile seed "$seed" \
    --slurpfile semantic "$semantic" '
      ($cargo_rail[0].remote_hits) as $cargo_rail_hits
      | ($seed[0].remote_publications) as $published_actions
      | {
          schema_version: 1,
          workload: $workload,
          round: $round,
          gate: "all_published_actions_restored_remotely_and_strictly_contain_safe_sccache_hits",
          cargo_rail_published_actions: $published_actions,
          cargo_rail_verified_remote_hits: $cargo_rail_hits,
          safe_sccache_remote_hits: $semantic[0].safe_sccache_hits,
          unsafe_sccache_hits: $semantic[0].unsafe_sccache_hits,
          semantic_coverage: $semantic[0],
          passed: (
            $published_actions > 0
            and $cargo_rail_hits == $published_actions
            and $semantic[0].passed
            and $semantic[0].cargo_rail_verified_hits == $cargo_rail_hits
          )
        }
    ' >"$directory/coverage.json"
  jq -e '.passed' "$directory/coverage.json" >/dev/null || {
    echo "official-S3 performance remote coverage gate failed: $workload/round-$round" >&2
    jq . "$directory/coverage.json" >&2
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
    verify_remote_coverage "$workload" "$round"
  done
done

find "$results/raw" -type f -name sample.json -print0 | sort -z | xargs -0 jq -sc . >"$results/samples.json"
find "$results/raw" -type f -name coverage.json -print0 | sort -z | xargs -0 jq -sc . >"$results/coverage.json"
jq -n \
  --argjson runs "$runs" \
  --argjson smoke "$([[ "$smoke" == 1 ]] && echo true || echo false)" \
  --slurpfile samples "$results/samples.json" \
  --slurpfile coverage "$results/coverage.json" '
  def quantile($values; $p):
    ($values | sort) as $sorted
    | if ($sorted | length) == 0 then null else $sorted[((($sorted | length) - 1) * $p | floor)] end;
  def metric($workload; $lane):
    [$samples[0][] | select(.accepted and .workload == $workload and .lane == $lane)] as $selected
    | [$selected[].measurement.elapsed_seconds] as $elapsed
    | {
        workload: $workload,
        lane: $lane,
        accepted_samples: ($selected | length),
        p50_elapsed_seconds: quantile($elapsed; 0.50),
        p95_elapsed_seconds: quantile($elapsed; 0.95),
        mean_elapsed_seconds: ($elapsed | add / length),
        min_elapsed_seconds: ($elapsed | min),
        max_elapsed_seconds: ($elapsed | max),
        total_user_seconds: ([$selected[].measurement.user_seconds] | add),
        total_system_seconds: ([$selected[].measurement.system_seconds] | add),
        max_rss_bytes: ([$selected[].measurement.max_rss_bytes] | max)
      };
  def reduction($baseline; $candidate): 100 * ($baseline - $candidate) / $baseline;
  ["check", "build", "test"] as $workloads
  | ["cargo-rail-s3", "sccache-s3"] as $lanes
  | [$workloads[] as $workload | $lanes[] as $lane | metric($workload; $lane)] as $metrics
  | {
      schema_version: 2,
      smoke: $smoke,
      required_accepted_samples: $runs,
      interleaving: "deterministic rotation by workload and round",
      accepted_samples: ([$samples[0][] | select(.accepted)] | length),
      rejected_samples: ([$samples[0][] | select(.accepted | not)] | length),
      remote_coverage: $coverage[0],
      metrics: $metrics,
      transport: [
        $workloads[] as $workload
        | {
            workload: $workload,
            cargo_rail: {
              remote_request_attempts: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.remote_request_attempts] | add),
              remote_coordinator_requests: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.remote_coordinator_requests] | add),
              remote_payload_bytes_read: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.remote_payload_bytes_read] | add),
              remote_payload_bytes_written: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.remote_payload_bytes_written] | add),
              remote_service_elapsed_ns: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.remote_service_elapsed_ns] | add),
              timing: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.timing]
                | reduce .[] as $sample ({};
                    reduce ($sample | to_entries[]) as $phase (.;
                      .[$phase.key].count = ((.[$phase.key].count // 0) + $phase.value.count)
                      | .[$phase.key].elapsed_ns =
                          ((.[$phase.key].elapsed_ns // 0) + $phase.value.elapsed_ns)))),
              durability: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "cargo-rail-s3")
                | .cache.durability]
                | reduce .[] as $sample ({};
                    reduce ($sample | to_entries[]) as $phase (.;
                      .[$phase.key].count = ((.[$phase.key].count // 0) + $phase.value.count)
                      | .[$phase.key].elapsed_ns =
                          ((.[$phase.key].elapsed_ns // 0) + $phase.value.elapsed_ns))))
            },
            sccache: {
              cache_hits: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "sccache-s3")
                | [.cache.stats.cache_hits.counts[]] | add // 0] | add),
              cache_misses: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "sccache-s3")
                | [.cache.stats.cache_misses.counts[]] | add // 0] | add),
              cache_read_errors: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "sccache-s3")
                | .cache.stats.cache_read_errors] | add),
              cache_writes: ([$samples[0][]
                | select(.accepted and .workload == $workload and .lane == "sccache-s3")
                | .cache.stats.cache_writes] | add)
            }
          }
      ],
      comparisons: [
        $workloads[] as $workload
        | ($metrics[] | select(.workload == $workload and .lane == "cargo-rail-s3")) as $cargo_rail
        | ($metrics[] | select(.workload == $workload and .lane == "sccache-s3")) as $sccache
        | {
            workload: $workload,
            cargo_rail_vs_sccache_p50_reduction_percent:
              reduction($sccache.p50_elapsed_seconds; $cargo_rail.p50_elapsed_seconds),
            cargo_rail_vs_sccache_p95_reduction_percent:
              reduction($sccache.p95_elapsed_seconds; $cargo_rail.p95_elapsed_seconds)
          }
      ]
    }
  | .target_qualified = (
      ($smoke | not)
      and $runs >= 20 and $runs <= 30
      and .accepted_samples == (6 * $runs)
      and (.remote_coverage | length) == (3 * $runs)
      and (.remote_coverage | all(.passed))
      and (.comparisons | all(
        .cargo_rail_vs_sccache_p50_reduction_percent >= 15
        and .cargo_rail_vs_sccache_p95_reduction_percent >= 15
      ))
    )
  | .superiority_qualified = (
      ($smoke | not)
      and $runs >= 20 and $runs <= 30
      and .accepted_samples == (6 * $runs)
      and (.remote_coverage | length) == (3 * $runs)
      and (.remote_coverage | all(.passed))
      and (.comparisons | all(
        .cargo_rail_vs_sccache_p50_reduction_percent >= 10
        and .cargo_rail_vs_sccache_p95_reduction_percent >= 10
      ))
    )
  ' >"$results/summary.json"

git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$results/worktree-status.txt"
capture_worktree_patch "$results/worktree.diff"
{
  printf '%s  %s\n' \
    "$(sha256_file "$repo_root/scripts/ci/qualify-native-cache-s3-performance.sh")" \
    scripts/ci/qualify-native-cache-s3-performance.sh
  printf '%s  %s\n' "$(sha256_file "$measure")" scripts/bench/measure-command.py
  printf '%s  %s\n' \
    "$(sha256_file "$coverage_reporter")" \
    scripts/bench/native-cache-coverage.py
  printf '%s  %s\n' \
    "$(sha256_file "$repo_root/scripts/fixtures/materialize-native-cache.sh")" \
    scripts/fixtures/materialize-native-cache.sh
} >"$results/harness-sha256.txt"

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg run_id "$run_id" \
  --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --arg worktree_diff_sha256 "$(sha256_file "$results/worktree.diff")" \
  --arg worktree_status_sha256 "$(sha256_file "$results/worktree-status.txt")" \
  --arg binary_sha256 "$(sha256_file "$binary")" \
  --arg harness_sha256 "$(sha256_file "$results/harness-sha256.txt")" \
  --arg rustc "$(rustc -vV)" \
  --arg cargo "$(cargo -Vv)" \
  --arg host "$(uname -a)" \
  --arg instance_type "${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
  --arg target "${DEV_MACHINE_TARGET:-unknown}" \
  --arg authority "$(jq -r '.remote.authority' <<<"$normalized")" \
  --arg sccache "$sccache_version" \
  '{
    schema_version: 2,
    generated_at: $generated_at,
    run_id: $run_id,
    repository_commit: $commit,
    worktree_diff_sha256: $worktree_diff_sha256,
    worktree_status_sha256: $worktree_status_sha256,
    release_binary_sha256: $binary_sha256,
    benchmark_harness_sha256: $harness_sha256,
    rustc: $rustc,
    cargo: $cargo,
    host: $host,
    dev_machine_target: $target,
    instance_type: $instance_type,
    remote: {provider: "aws-s3", authority: $authority, credentials: "ec2-instance-profile"},
    sccache: $sccache,
    sccache_startup_requirement:
      "pre-existing .sccache_check object when the client role intentionally lacks s3:ListBucket"
  }' >"$results/environment.json"

jq . "$results/summary.json"
if [[ "$smoke" == 0 ]] && ! jq -e '.superiority_qualified' "$results/summary.json" >/dev/null; then
  echo "official-S3 performance qualification did not reach the required 10% p50/p95 superiority" >&2
  exit 1
fi
echo "official-S3 performance result: $results"
