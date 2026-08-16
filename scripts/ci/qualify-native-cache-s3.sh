#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
measure="$repo_root/scripts/bench/measure-command.py"

usage() {
  echo "usage: $0 <producer|consumer> <run-id> <official-remote-url>" >&2
  exit 2
}

phase="${1:-}"
run_id="${2:-}"
remote_url="${3:-}"
[[ "$phase" == producer || "$phase" == consumer ]] || usage
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
[[ "$remote_url" == s3://* || "$remote_url" == azure://* || "$remote_url" == r2://* ]] || usage
[[ "$#" -eq 3 ]] || usage

for tool in cargo git jq python3; do
  command -v "$tool" >/dev/null || {
    echo "remote-cache qualification requires $tool" >&2
    exit 2
  }
done

binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
if [[ -z "${CARGO_RAIL_BIN+x}" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
fi
[[ -x "$binary" ]] || {
  echo "remote-cache qualification binary is not executable: $binary" >&2
  exit 2
}
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"

normalized="$($binary rail cache normalize "$remote_url" --mode "$([[ "$phase" == producer ]] && echo read-write || echo read)" -f json)"
jq -e '(.remote.provider == "aws-s3" or .remote.provider == "azure-blob" or .remote.provider == "cloudflare-r2") and .remote.activation == "direct_transport_selected"' <<<"$normalized" >/dev/null || {
  echo "remote qualification requires a normalized AWS, Azure, or R2 authority" >&2
  exit 2
}
remote_provider="$(jq -r '.remote.provider' <<<"$normalized")"
credential_model=ec2-instance-profile
[[ "$remote_provider" == azure-blob ]] && credential_model=azure-passwordless-chain
[[ "$remote_provider" == cloudflare-r2 ]] && credential_model=r2-api-token
normalized_url="$(jq -r '.normalized_url' <<<"$normalized")"
location="${normalized_url%%\?*}"
authority="${location#*://}"
case "$remote_provider" in
  aws-s3)
    provider_slug=s3
    bucket="${authority%%/*}"
    prefix="${authority#*/}"
    [[ "$bucket" != "$authority" ]] || usage
    ;;
  azure-blob)
    provider_slug=azure
    account="${authority%%/*}"
    remainder="${authority#*/}"
    container="${remainder%%/*}"
    prefix="${remainder#*/}"
    [[ "$account" =~ ^[a-z0-9]{3,24}$ && "$container" != "$remainder" ]] || usage
    [[ "$run_id" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || {
      echo "Azure Blob qualification requires a lowercase DNS-safe run ID" >&2
      exit 2
    }
    expected_container="cargo-rail-task9-$run_id"
    [[ "${#expected_container}" -le 63 && "$container" == "$expected_container" ]] || {
      echo "Azure Blob qualification requires disposable container $expected_container" >&2
      exit 2
    }
    ;;
  cloudflare-r2)
    provider_slug=r2
    account="${authority%%/*}"
    remainder="${authority#*/}"
    bucket="${remainder%%/*}"
    prefix="${remainder#*/}"
    [[ "$account" =~ ^[0-9a-f]{32}$ && "$bucket" != "$remainder" ]] || usage
    ;;
  *) usage ;;
esac
expected_prefix="cache/cargo-rail/$provider_slug/v3/task9/$run_id"
[[ "$prefix" == "$expected_prefix" ]] || {
  echo "remote qualification requires exact disposable prefix $expected_prefix" >&2
  exit 2
}

results="$repo_root/benchmark_results/native-cache/$run_id-$phase"
state="$repo_root/benchmark_results/native-cache-s3-state/$run_id"
[[ ! -e "$results" ]] || {
  echo "remote-cache qualification result already exists: $results" >&2
  exit 2
}
[[ ! -e "$state" ]] || {
  echo "remote-cache qualification state already exists on this machine: $state" >&2
  exit 2
}
mkdir -p "$results/raw" "$state/cargo-homes" "$state/caches" "$state/fixtures"
chmod 700 "$results" "$results/raw" "$state" "$state/cargo-homes" "$state/caches" "$state/fixtures"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

capture_worktree_patch() {
  local output="$1"
  git -C "$repo_root" diff --binary HEAD -- >"$output"
  while IFS= read -r -d '' untracked; do
    [[ -f "$repo_root/$untracked" || -L "$repo_root/$untracked" ]] || {
      echo "remote-cache evidence cannot capture non-file untracked input: $untracked" >&2
      return 1
    }
    local status=0
    git -C "$repo_root" diff --binary --no-index -- /dev/null "$untracked" >>"$output" || status=$?
    [[ "$status" -eq 1 ]] || {
      echo "remote-cache evidence could not capture untracked input: $untracked" >&2
      return 1
    }
  done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
}

shared_git="$state/fixture-git-source"
workloads=(check build test)
setup_mode=read
[[ "$phase" == producer ]] && setup_mode=read-write
source_cargo_home="${CARGO_HOME:-${HOME:?HOME is required}/.cargo}"
source_cargo_home="$(cd "$source_cargo_home" && pwd -P)"
for workload in "${workloads[@]}"; do
  root="$state/fixtures/$workload"
  cargo_home="$state/cargo-homes/$workload"
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
  mkdir -p "$state/caches/$workload"
  (
    cd "$root"
    env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$cargo_home" \
      "$binary" rail cache setup --local-dir "$state/caches/$workload" --max-size 10GiB \
        --remote "$remote_url" --remote-mode "$setup_mode" >/dev/null
  )
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
      echo "remote-cache Cargo output is not one target-contained regular file: $path" >&2
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
      find "$target" -type f ! -path '*/incremental/*' \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -print
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
      hit_remote_request_attempts: ([.[] | select(.status == "hit") | .remote_request_attempts] | add // 0),
      hit_remote_coordinator_requests: ([.[] | select(.status == "hit") | .remote_coordinator_requests] | add // 0),
      read_only_remote_misses: ([.[] | select(
        .status == "miss" and .reason == "remote_entry_not_found;stored_verified_result;remote_read_only"
      )] | length),
      remote_publications: ([.[] | select(.reason | contains("remote_published"))] | length),
      published_action_ids: ([.[] | select(.reason | contains("remote_published")) | .action_id] | sort),
      remote_hit_action_ids: ([.[] | select(
        .status == "hit" and .reason == "verified_remote_result"
      ) | .action_id] | sort),
      local_hit_action_ids: ([.[] | select(
        .status == "hit" and .reason == "verified_local_result"
      ) | .action_id] | sort),
      rust_class_counts: (reduce [.[].action.action_class][] as $class ({};
        .[$class] = ((.[$class] // 0) + 1))),
      remote_request_attempts: ([.[].remote_request_attempts] | add // 0),
      remote_coordinator_requests: ([.[].remote_coordinator_requests] | add // 0),
      remote_payload_bytes_read: ([.[].remote_payload_bytes_read] | add // 0),
      remote_payload_bytes_written: ([.[].remote_payload_bytes_written] | add // 0),
      coordinator_compressed_ipc_bytes: ([.[].coordinator_compressed_ipc_bytes] | add // 0),
      packed_l1_bytes_written: ([.[].packed_l1_bytes_written] | add // 0),
      avoided_materialized_l1_bytes: ([.[].avoided_materialized_l1_bytes] | add // 0),
      output_handoff_bytes: ([.[].output_handoff_bytes] | add // 0),
      remote_errors: ([.[].remote_error? | select(. != null)] | unique)
    }' "${events[@]}"
}

run_workload() {
  local workload="$1"
  local label="$2"
  local endpoint_override="${3:-}"
  local root="$state/fixtures/$workload"
  local cargo_home="$state/cargo-homes/$workload"
  local directory="$results/raw/$workload/$label"
  local mode=read
  [[ "$phase" == producer ]] && mode=read-write
  rm -rf -- "$root/target"
  mkdir -p "$directory/events"
  chmod 700 "$directory" "$directory/events"
  local -a command=(cargo "$workload")
  [[ "$workload" == build ]] && command+=(--release)
  [[ "$workload" == test ]] && command+=(--no-run --all-targets)
  command+=(--workspace --all-features --locked --offline --message-format=json-render-diagnostics)
  local -a measurement=(
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
    --env "CARGO_HOME=$cargo_home"
    --env CARGO_TERM_COLOR=never
    --env CARGO_NET_OFFLINE=true
    --env CARGO_INCREMENTAL=0
    --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1
    --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$directory/events"
  )
  if [[ -n "$endpoint_override" ]]; then
    measurement+=(--env "AWS_ENDPOINT_URL_S3=$endpoint_override")
  else
    measurement+=(--unset AWS_ENDPOINT_URL_S3 --unset AWS_ENDPOINT_URL)
  fi
  python3 "${measurement[@]}" -- "${command[@]}"
  manifest_outputs "$root" "$directory/stdout" "$directory/outputs.sha256"
  [[ -s "$directory/outputs.sha256" ]] || {
    echo "remote-cache qualification produced no outputs: $workload/$label" >&2
    exit 1
  }
  event_summary "$directory/events" >"$directory/events.json"
}

for workload in "${workloads[@]}"; do
  run_workload "$workload" "$phase"
  summary="$results/raw/$workload/$phase/events.json"
  if [[ "$phase" == producer ]]; then
    jq -e '
      .events > 0
      and .misses > 0
      and .remote_publications > 0
      and (.remote_request_attempts > 0 or .remote_coordinator_requests > 0)
      and .remote_payload_bytes_written > 0
      and (.remote_errors | length) == 0
    ' "$summary" >/dev/null || {
      echo "remote-cache producer did not publish verified remote results: $workload" >&2
      exit 1
    }
  else
    jq -e '
      .events > 0
      and .hits > 0
      and .misses == .read_only_remote_misses
      and .remote_hits > 0
      and (.remote_request_attempts > 0 or .remote_coordinator_requests > 0)
      and .remote_payload_bytes_read > 0
      and .remote_payload_bytes_written == 0
      and (.remote_errors | length) == 0
    ' "$summary" >/dev/null || {
      echo "remote-cache consumer did not import verified remote results without writes: $workload" >&2
      exit 1
    }
    endpoint_override=""
    [[ "$remote_provider" == aws-s3 ]] && endpoint_override=http://127.0.0.1:1
    run_workload "$workload" l1-offline "$endpoint_override"
    offline="$results/raw/$workload/l1-offline/events.json"
    jq -e '
      .events > 0
      and .hits > 0
      and .hit_remote_request_attempts == 0
      and .hit_remote_coordinator_requests == 0
      and .remote_payload_bytes_read == 0
      and .remote_payload_bytes_written == 0
      and (($remote[0].remote_hit_action_ids - .local_hit_action_ids) | length) == 0
    ' --slurpfile remote "$summary" "$offline" >/dev/null || {
      echo "remote-cache consumer L1 hit attempted remote access: $workload" >&2
      exit 1
    }
    cmp "$results/raw/$workload/$phase/outputs.sha256" "$results/raw/$workload/l1-offline/outputs.sha256" || {
      echo "remote-cache consumer L1 outputs changed during remote outage: $workload" >&2
      exit 1
    }
  fi
done

jq -n \
  --slurpfile check "$results/raw/check/$phase/events.json" \
  --slurpfile build "$results/raw/build/$phase/events.json" \
  --slurpfile test "$results/raw/test/$phase/events.json" '
  [
    "binary",
    "build_script",
    "c_dynamic_library",
    "proc_macro_producer",
    "rust_dynamic_library",
    "rust_library",
    "static_library",
    "test"
  ] as $required
  | [$check[0].rust_class_counts, $build[0].rust_class_counts, $test[0].rust_class_counts]
  | (reduce (.[] | to_entries[]) as $entry ({};
      .[$entry.key] = ((.[$entry.key] // 0) + $entry.value))) as $counts
  | ($counts | to_entries | map(select(.value > 0) | .key) | sort) as $observed
  | ($required - $observed) as $missing
  | {
      schema_version: 1,
      complete: ($missing | length) == 0,
      required: $required,
      observed: $observed,
      missing: $missing,
      counts: $counts
    }
' >"$results/compiler-class-coverage.json"
jq -e '.complete' "$results/compiler-class-coverage.json" >/dev/null || {
  echo "remote-cache qualification did not exercise every required compiler class" >&2
  jq . "$results/compiler-class-coverage.json" >&2
  exit 1
}

git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$results/worktree-status.txt"
capture_worktree_patch "$results/worktree.diff"
printf '%s  %s\n' \
  "$(sha256_file "$repo_root/scripts/ci/qualify-native-cache-s3.sh")" \
  scripts/ci/qualify-native-cache-s3.sh >"$results/harness-sha256.txt"
printf '%s  %s\n' "$(sha256_file "$measure")" scripts/bench/measure-command.py >>"$results/harness-sha256.txt"
printf '%s  %s\n' \
  "$(sha256_file "$repo_root/scripts/fixtures/materialize-native-cache.sh")" \
  scripts/fixtures/materialize-native-cache.sh >>"$results/harness-sha256.txt"

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg phase "$phase" \
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
  --arg provider "$remote_provider" \
  --arg authority "$(jq -r '.remote.authority' <<<"$normalized")" \
  --arg mode "$(jq -r '.remote.mode' <<<"$normalized")" \
  --arg credentials "$credential_model" \
  '{
    schema_version: 2,
    generated_at: $generated_at,
    phase: $phase,
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
    remote: {
      provider: $provider,
      authority: $authority,
      mode: $mode,
      credentials: $credentials
    }
  }' >"$results/environment.json"

offline_check=null
offline_build=null
offline_test=null
if [[ "$phase" == consumer ]]; then
  offline_check="$(jq -c . "$results/raw/check/l1-offline/events.json")"
  offline_build="$(jq -c . "$results/raw/build/l1-offline/events.json")"
  offline_test="$(jq -c . "$results/raw/test/l1-offline/events.json")"
fi

jq -n \
  --arg phase "$phase" \
  --slurpfile check "$results/raw/check/$phase/events.json" \
  --slurpfile build "$results/raw/build/$phase/events.json" \
  --slurpfile test "$results/raw/test/$phase/events.json" \
  --slurpfile classes "$results/compiler-class-coverage.json" \
  --argjson offline_check "$offline_check" \
  --argjson offline_build "$offline_build" \
  --argjson offline_test "$offline_test" \
  '{
    schema_version: 2,
    phase: $phase,
    workloads: {
      check: {primary: $check[0], l1_offline: $offline_check},
      build: {primary: $build[0], l1_offline: $offline_build},
      test: {primary: $test[0], l1_offline: $offline_test}
    },
    compiler_class_coverage: $classes[0],
    passed: true
  }' >"$results/result.json"

echo "remote-cache qualification result: $results"
