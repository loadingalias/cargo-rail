#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
reporter="$repo_root/scripts/bench/native-cache-report.sh"
command -v jq >/dev/null || {
  echo "missing required benchmark tool: jq" >&2
  exit 2
}

usage() {
  echo "usage: $0 <runs>|run <runs>|smoke|resume <result-directory>" >&2
  exit 2
}

operation="${1:-}"
case "$operation" in
  run)
    shift
    [[ "$#" -eq 1 ]] || usage
    runs="$1"
    evidence_kind=retained
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
  smoke)
    shift
    [[ "$#" -eq 0 ]] || {
      echo "usage: $0 smoke" >&2
      exit 2
    }
    runs=1
    evidence_kind=smoke
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/smoke-$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
  resume)
    shift
    results="${1:-${CARGO_RAIL_BENCH_RESULTS:-}}"
    [[ -n "$results" && "$#" -le 1 ]] || {
      echo "usage: $0 resume <result-directory>" >&2
      exit 2
    }
    results="$(cd "$results" 2>/dev/null && pwd -P)" || {
      echo "native-cache result directory does not exist: $results" >&2
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
  [0-9]*)
    [[ "$#" -eq 1 ]] || usage
    runs="$operation"
    operation=run
    evidence_kind=retained
    results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"
    ;;
esac

binary_overridden="${CARGO_RAIL_BIN+x}"
binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
sccache_expected_version="$(sed -n 's/^readonly SCCACHE_VERSION=//p' "$repo_root/scripts/ci/install-tools.sh")"
[[ "$sccache_expected_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "cannot read the pinned sccache version from scripts/ci/install-tools.sh" >&2
  exit 2
}

[[ "$runs" =~ ^[0-9]+$ ]] || {
  echo "native-cache benchmark runs must be an integer" >&2
  exit 2
}
if [[ "$runs" -lt 1 ]]; then
  echo "native-cache benchmark runs must be positive" >&2
  exit 2
fi
if [[ "$operation" == resume ]]; then
  attempts="$(jq -er '.attempts' "$results/run.json")"
  warmup_runs="$(jq -er '.warmup_runs' "$results/run.json")"
elif [[ "$evidence_kind" == smoke ]]; then
  attempts=1
  warmup_runs=0
else
  attempts="$runs"
  warmup_runs=1
fi
if [[ "$operation" != resume ]]; then
  mkdir -p "$results"
  results="$(cd "$results" && pwd -P)"
  [[ -z "$(find "$results" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
    echo "native-cache result directory is not empty; use resume or choose another directory: $results" >&2
    exit 2
  }
fi
run_id="$(basename "$results")"

for tool in cargo git hyperfine; do
  command -v "$tool" >/dev/null || {
    echo "missing required benchmark tool: $tool" >&2
    exit 2
  }
done
if [[ -z "$binary_overridden" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins \
    --all-features --release --locked
fi
[[ -f "$binary" && -x "$binary" ]] || {
  echo "build the release binary first: cargo build --release --locked" >&2
  exit 2
}
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) wrapper_binary="$(dirname "$binary")/cargo-rail-native-rustc-wrapper.exe" ;;
  *) wrapper_binary="$(dirname "$binary")/cargo-rail-native-rustc-wrapper" ;;
esac
[[ -f "$wrapper_binary" && -x "$wrapper_binary" ]] || {
  echo "build the release compiler wrapper beside cargo-rail: cargo build --release --bins --locked" >&2
  exit 2
}

unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
unset CARGO_INCREMENTAL CARGO_TARGET_DIR RUSTC RUSTC_BOOTSTRAP RUSTC_FORCE_INCREMENTAL RUSTC_WORKSPACE_WRAPPER
unset HYPERFINE_ITERATION HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET SHLVL __CF_USER_TEXT_ENCODING
unset RUSTC_WRAPPER RUSTFLAGS
unset SCCACHE_AZURE_BLOB_CONTAINER SCCACHE_AZURE_CONNECTION_STRING SCCACHE_AZURE_KEY_PREFIX
unset SCCACHE_BUCKET SCCACHE_COS_BUCKET SCCACHE_ENDPOINT SCCACHE_GCS_BUCKET SCCACHE_GHA_ENABLED SCCACHE_GHA_VERSION
unset SCCACHE_MEMCACHED SCCACHE_MEMCACHED_ENDPOINT SCCACHE_MULTILEVEL_CHAIN SCCACHE_OSS_BUCKET
unset SCCACHE_REDIS SCCACHE_REDIS_CLUSTER_ENDPOINTS SCCACHE_REDIS_ENDPOINT SCCACHE_REGION SCCACHE_S3_KEY_PREFIX
unset SCCACHE_WEBDAV_ENDPOINT SCCACHE_CLIENT_SIDE

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

l2_alias="${CARGO_RAIL_BENCH_L2_ALIAS:-}"
l2_targets_file="${CARGO_RAIL_BENCH_L2_TARGETS_FILE:-}"
l2_enabled=false
if [[ -n "$l2_alias" || -n "$l2_targets_file" ]]; then
  [[ -n "$l2_alias" && -n "$l2_targets_file" ]] || {
    echo "native-cache L2 measurement requires both CARGO_RAIL_BENCH_L2_ALIAS and CARGO_RAIL_BENCH_L2_TARGETS_FILE" >&2
    exit 2
  }
  [[ "$l2_alias" =~ ^[a-z][a-z0-9_-]{0,63}$ && "$l2_targets_file" = /* && -f "$l2_targets_file" ]] || {
    echo "native-cache L2 measurement authority is invalid" >&2
    exit 2
  }
  if ! jq -e --arg alias "$l2_alias" '
    .version == 1
    and .targets[$alias].protocol == "s3"
    and .targets[$alias].role == "read_write"
    and (.targets[$alias].shareable_environment | index("CARGO_PKG_VERSION") != null)
  ' "$l2_targets_file" >/dev/null; then
    echo "native-cache L2 measurement requires a read-write S3 target that permits CARGO_PKG_VERSION" >&2
    exit 2
  fi
  l2_enabled=true
fi
l2_targets_sha256=""
if [[ "$l2_enabled" == true ]]; then
  l2_targets_sha256="$(sha256_file "$l2_targets_file")"
fi

tree_content_sha256() {
  local root="$1"
  local manifest="$2"
  local path relative
  : >"$manifest"
  while IFS= read -r -d '' path; do
    relative="${path#"$root"/}"
    printf '%q  ' "$relative" >>"$manifest"
    if [[ -L "$path" ]]; then
      printf 'symlink:%q\n' "$(readlink "$path")" >>"$manifest"
    else
      printf 'file:%s\n' "$(sha256_file "$path")" >>"$manifest"
    fi
  done < <(find "$root" \( -type f -o -type l \) -print0 | sort -z)
  sha256_file "$manifest"
}

binary_sha256="$(sha256_file "$binary")"
wrapper_binary_sha256="$(sha256_file "$wrapper_binary")"
worktree_dirty=false
[[ -z "$(git -C "$repo_root" status --porcelain=v1)" ]] || worktree_dirty=true
identity_root="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-native-identity.XXXXXX")"
trap 'rm -rf -- "${identity_root:-}"' EXIT
status_file="$identity_root/worktree-status.txt"
git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$status_file"
worktree_status_sha256="$(sha256_file "$status_file")"
diff_file="$identity_root/worktree.diff"
git -C "$repo_root" diff --binary HEAD -- >"$diff_file"
worktree_diff_sha256="$(sha256_file "$diff_file")"
harness_manifest="$identity_root/harness-sha256.txt"
for harness in \
  scripts/bench/native-cache.sh \
  scripts/bench/native-cache-report.sh \
  scripts/bench/native-cache-summary.jq \
  scripts/bench/native-cache-validate.jq \
  scripts/bench/linux-process-forest-rss.py \
  scripts/bench/rustc-timing-wrapper.sh \
  scripts/fixtures/materialize-native-cache.sh; do
  printf '%s  %s\n' "$(sha256_file "$repo_root/$harness")" "$harness" >>"$harness_manifest"
done
harness_sha256="$(sha256_file "$harness_manifest")"
untracked_manifest="$identity_root/untracked-sha256.txt"
: >"$untracked_manifest"
while IFS= read -r -d '' untracked; do
  printf '%q  ' "$untracked" >>"$untracked_manifest"
  if [[ -L "$repo_root/$untracked" ]]; then
    printf 'symlink:%q\n' "$(readlink "$repo_root/$untracked")" >>"$untracked_manifest"
  elif [[ -f "$repo_root/$untracked" ]]; then
    printf 'file:%s\n' "$(sha256_file "$repo_root/$untracked")" >>"$untracked_manifest"
  else
    printf 'other\n' >>"$untracked_manifest"
  fi
done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
untracked_sha256="$(sha256_file "$untracked_manifest")"
filesystem="${CARGO_RAIL_BENCH_FILESYSTEM:-}"
if [[ -z "$filesystem" ]]; then
  case "$(uname -s)" in
    Darwin) filesystem="$(diskutil info / 2>/dev/null | sed -n 's/^[[:space:]]*File System Personality:[[:space:]]*//p' | head -1)" ;;
    Linux) filesystem="$(stat -f -c %T "$repo_root" 2>/dev/null || true)" ;;
    MINGW* | MSYS* | CYGWIN*) filesystem="$(fsutil fsinfo volumeinfo "${SYSTEMDRIVE:-C:}" 2>/dev/null | tr '\r\n' ' ' || true)" ;;
  esac
fi
host_cpu="${CARGO_RAIL_BENCH_HOST_CPU:-}"
if [[ -z "$host_cpu" ]]; then
  case "$(uname -s)" in
    Darwin) host_cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)" ;;
    Linux) host_cpu="$(lscpu 2>/dev/null | sed -n 's/^Model name:[[:space:]]*//p' | head -1)" ;;
  esac
fi

environment_candidate="$identity_root/environment.json"
jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg fixture "tests/fixtures/native_cache/real_world" \
  --arg binary "$binary" \
  --arg binary_sha256 "$binary_sha256" \
  --arg wrapper_binary "$wrapper_binary" \
  --arg wrapper_binary_sha256 "$wrapper_binary_sha256" \
  --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --argjson worktree_dirty "$worktree_dirty" \
  --arg worktree_status_sha256 "$worktree_status_sha256" \
  --arg worktree_diff_sha256 "$worktree_diff_sha256" \
  --arg untracked_sha256 "$untracked_sha256" \
  --arg harness_sha256 "$harness_sha256" \
  --arg rustc "$(rustc -vV)" \
  --arg cargo "$(cargo -Vv)" \
  --arg host "$(uname -a)" \
  --arg host_cpu "$host_cpu" \
  --arg instance_type "${CARGO_RAIL_BENCH_INSTANCE_TYPE:-local}" \
  --arg filesystem "$filesystem" \
  --argjson l2_enabled "$l2_enabled" \
  --arg l2_alias "$l2_alias" \
  --arg l2_targets_sha256 "$l2_targets_sha256" \
  '{
    schema_version: 8,
    generated_at: $generated_at,
    fixture: $fixture,
    release_binary: $binary,
    release_binary_sha256: $binary_sha256,
    compiler_wrapper_binary: $wrapper_binary,
    compiler_wrapper_binary_sha256: $wrapper_binary_sha256,
    repository_commit: $commit,
    worktree_dirty: $worktree_dirty,
    worktree_status_sha256: $worktree_status_sha256,
    worktree_diff_sha256: $worktree_diff_sha256,
    untracked_sha256: $untracked_sha256,
    benchmark_harness_sha256: $harness_sha256,
    rustc: $rustc,
    cargo: $cargo,
    host: $host,
    host_cpu: (if $host_cpu == "" then null else $host_cpu end),
    instance_type: $instance_type,
    filesystem: (if $filesystem == "" then null else $filesystem end),
    l2: {
      enabled: $l2_enabled,
      alias: (if $l2_alias == "" then null else $l2_alias end),
      target_map_sha256: (if $l2_targets_sha256 == "" then null else ("sha256:" + $l2_targets_sha256) end),
      workload: (if $l2_enabled then "single eligible fixture library" else null end),
      sampling: (if $l2_enabled then "one publish, one empty-L1 hit, one unavailable-L2 fallback" else null end)
    },
    environment: {
      cargo_incremental: "clean/native-cache/sccache=0; manifest profile for warm/delegated; explicit bypass pair=1",
      network: (if $l2_enabled then "Cargo offline; pinned S3 authority enabled only for the L2 cycle" else "offline" end),
      cargo_target_dir: "fixture-local; empty per clean lane; retained per warm/delegated/bypass lane",
      compiler_overrides: "removed",
      benchmark_control_environment: "Hyperfine and nested-shell control variables removed before compiler environment capture",
      rustflags: "removed",
      rustc_wrapper: "removed_except_sccache_lane",
      rustc_workspace_wrapper: "removed",
      diagnostics: "disabled in timed commands; unmeasured Cargo-Rail cold/warm explain replay only",
      output: "timed Cargo JSON plus compact cache decision; unmeasured isolated explain replay for diagnostics and cache evidence"
    }
  }' >"$environment_candidate"

if [[ "$operation" == resume ]]; then
  [[ -f "$results/environment.json" ]] || {
    echo "native-cache result is not resumable because environment.json is missing: $results" >&2
    exit 2
  }
  if ! jq -e -n --slurpfile recorded "$results/environment.json" --slurpfile current "$environment_candidate" \
    '($recorded[0] | del(.generated_at)) == ($current[0] | del(.generated_at))' >/dev/null; then
    echo "native-cache resume identity changed; start a new result directory: $results" >&2
    jq -n --slurpfile recorded "$results/environment.json" --slurpfile current "$environment_candidate" \
      '{recorded: ($recorded[0] | del(.generated_at)), current: ($current[0] | del(.generated_at))}' >&2
    exit 2
  fi
else
  mv "$environment_candidate" "$results/environment.json"
  mv "$status_file" "$results/worktree-status.txt"
  mv "$diff_file" "$results/worktree.diff"
  mv "$untracked_manifest" "$results/untracked-sha256.txt"
fi
environment_sha256="$(sha256_file "$results/environment.json")"
rm -rf -- "$identity_root"
trap - EXIT

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-native-bench.XXXXXX")"
fixture_root="$(cd "$fixture_root" && pwd -P)"
cleanup() {
  if [[ -n "$fixture_root" && "$fixture_root" != / && "$fixture_root" == *cargo-rail-native-bench.* ]]; then
    rm -rf -- "$fixture_root"
  fi
}
trap cleanup EXIT

safe_remove() {
  local path
  for path in "$@"; do
    case "$path" in
      "$fixture_root"/*) rm -rf -- "$path" ;;
      *)
        echo "refusing to remove benchmark path outside fixture root: $path" >&2
        exit 2
        ;;
    esac
  done
}

shared_git="$fixture_root/git-source"
fixture_source_manifest="$fixture_root/fixture-source-sha256.txt"
fixture_source_sha256="$(tree_content_sha256 \
  "$repo_root/tests/fixtures/native_cache/real_world" \
  "$fixture_source_manifest")"
materialize() {
  "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$fixture_root/$1" "$shared_git" >/dev/null
}

lanes=(
  native-cargo
  native-cargo-warm
  native-cargo-incremental-warm
  cargo-rail-delegated
  cargo-rail-incremental-bypass
  cargo-rail-disabled
  cargo-rail-cold
  cargo-rail-warm
  sccache
  sccache-client-side
  cargo-rail-sccache-preserved
)
workloads=(check build)

lane_root() {
  local workload="$1"
  local lane="$2"
  case "$lane" in
    cargo-rail-cold | cargo-rail-warm) printf '%s\n' "$fixture_root/$workload-cargo-rail-cache" ;;
    *) printf '%s\n' "$fixture_root/$workload-$lane" ;;
  esac
}

for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    case "$lane" in
      cargo-rail-cold | cargo-rail-warm) ;;
      *) materialize "$workload-$lane" ;;
    esac
  done
  materialize "$workload-cargo-rail-cache"
  materialize "$workload-sccache-seed"
done
fixture_git_commit="$(git -C "$shared_git" rev-parse --verify HEAD)"
materialized_fixture_commit="$(git -C "$fixture_root/check-native-cargo" rev-parse --verify HEAD)"
materialized_fixture_tree="$(git -C "$fixture_root/check-native-cargo" rev-parse 'HEAD^{tree}')"
for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    root="$(lane_root "$workload" "$lane")"
    [[ "$(git -C "$root" rev-parse --verify HEAD)" == "$materialized_fixture_commit" ]] || {
      echo "materialized fixture commit differs for $workload/$lane" >&2
      exit 1
    }
    [[ "$(git -C "$root" rev-parse 'HEAD^{tree}')" == "$materialized_fixture_tree" ]] || {
      echo "materialized fixture tree differs for $workload/$lane" >&2
      exit 1
    }
  done
done
materialized_fixture_lock_sha256="$(sha256_file "$fixture_root/check-native-cargo/Cargo.lock")"
normalized_fixture_lock="$fixture_root/Cargo.lock.normalized"
sed -E 's#git\+file://[^?"]+#git+file://<FIXTURE_GIT>#g' \
  "$fixture_root/check-native-cargo/Cargo.lock" >"$normalized_fixture_lock"
fixture_lock_sha256="$(sha256_file "$normalized_fixture_lock")"

sccache_available=false
sccache_bin="${SCCACHE_BIN:-$(command -v sccache || true)}"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    if [[ "$sccache_bin" != *.exe && -f "$sccache_bin.exe" ]]; then
      sccache_bin="$sccache_bin.exe"
    fi
    if [[ -n "$sccache_bin" ]]; then
      sccache_bin="$(cygpath -m "$sccache_bin")"
    fi
    ;;
  *)
    if [[ -n "$sccache_bin" ]]; then
      sccache_bin="$(cd "$(dirname "$sccache_bin")" 2>/dev/null && pwd -P)/$(basename "$sccache_bin")"
    fi
    ;;
esac
sccache_version=""
sccache_unavailable_reason="sccache $sccache_expected_version was not found"
if [[ -n "$sccache_bin" && -x "$sccache_bin" ]]; then
  sccache_version="$("$sccache_bin" --version)"
  if [[ "$sccache_version" == "sccache $sccache_expected_version" ]]; then
    sccache_available=true
    sccache_unavailable_reason=""
  elif [[ -n "${SCCACHE_BIN:-}" ]]; then
    echo "SCCACHE_BIN must name sccache $sccache_expected_version: $sccache_bin reports $sccache_version" >&2
    exit 2
  else
    sccache_unavailable_reason="$sccache_bin reports $sccache_version, not sccache $sccache_expected_version"
    sccache_bin=""
  fi
fi

sccache_resource_accounting_available=false
sccache_resource_accounting_reason="complete detached-server accounting requires Linux /proc, Python 3, and GNU time"
if [[ "$(uname -s)" == Linux && -d /proc && -x /usr/bin/time ]] \
  && command -v python3 >/dev/null 2>&1 \
  && /usr/bin/time -f '%U %S %M' -o /dev/null true >/dev/null 2>&1; then
  sccache_resource_accounting_available=true
  sccache_resource_accounting_reason=""
fi

native_cache_capability_candidate="$fixture_root/native-cache-capability.json"
native_cache_capability_stderr_candidate="$fixture_root/native-cache-capability.stderr"
native_cache_doctor_environment=(env)
if [[ "$l2_enabled" == true ]]; then
  native_cache_doctor_environment+=("CARGO_RAIL_CACHE_TARGETS_FILE=$l2_targets_file")
fi
if ! (
  cd "$fixture_root/check-native-cargo"
  "${native_cache_doctor_environment[@]}" "$binary" rail doctor native-cache -f json \
    >"$native_cache_capability_candidate" 2>"$native_cache_capability_stderr_candidate"
); then
  echo "cargo-rail native-cache capability inspection failed" >&2
  sed -n '1,80p' "$native_cache_capability_stderr_candidate" >&2
  jq . "$native_cache_capability_candidate" >&2 || true
  exit 1
fi
if ! jq -e '
  .schema_version == 1
  and .command == "doctor"
  and .mode == "native_cache"
  and .result == "success"
  and .capability.schema_version == 6
  and .capability.cache_class == "library_metadata_rlib"
  and .capability.execution_contract == "direct-global-wrapper-v10"
  and (.capability.platform | type) == "string"
  and (.capability.host_target | type) == "string"
  and (.capability.identity | type) == "string"
  and (.capability.identity | test("^sha256:[0-9a-f]{64}$"))
  and (.capability | has("certified") | not)
  and (.capability | has("evidence") | not)
' "$native_cache_capability_candidate" >/dev/null; then
  echo "cargo-rail native-cache capability inspection returned an invalid contract" >&2
  exit 1
fi
cargo_rail_cache_platform="$(jq -r '.capability.platform' "$native_cache_capability_candidate")"
cargo_rail_cache_host_target="$(jq -r '.capability.host_target' "$native_cache_capability_candidate")"
cargo_rail_cache_identity="$(jq -r '.capability.identity' "$native_cache_capability_candidate")"
cargo_rail_cache_execution_contract="$(jq -r '.capability.execution_contract' "$native_cache_capability_candidate")"

run_candidate="$fixture_root/run.json"
lanes_json="$(printf '%s\n' "${lanes[@]}" | jq -Rsc 'split("\n")[:-1]')"
workloads_json="$(printf '%s\n' "${workloads[@]}" | jq -Rsc 'split("\n")[:-1]')"
jq -n \
  --arg run_id "$run_id" \
  --arg evidence_kind "$evidence_kind" \
  --argjson required_accepted_samples "$runs" \
  --argjson attempts "$attempts" \
  --argjson warmup_runs "$warmup_runs" \
  --argjson lanes "$lanes_json" \
  --argjson workloads "$workloads_json" \
  --arg fixture_source_sha256 "$fixture_source_sha256" \
  --arg fixture_git_commit "$fixture_git_commit" \
  --arg fixture_lock_sha256 "$fixture_lock_sha256" \
  --arg materialized_fixture_commit "$materialized_fixture_commit" \
  --arg materialized_fixture_tree "$materialized_fixture_tree" \
  --arg cargo_rail_cache_platform "$cargo_rail_cache_platform" \
  --arg cargo_rail_cache_host_target "$cargo_rail_cache_host_target" \
  --arg cargo_rail_cache_identity "$cargo_rail_cache_identity" \
  --arg cargo_rail_cache_execution_contract "$cargo_rail_cache_execution_contract" \
  --argjson sccache_available "$sccache_available" \
  --arg sccache_path "$sccache_bin" \
  --arg sccache_version "$sccache_version" \
  --arg sccache_unavailable_reason "$sccache_unavailable_reason" \
  --argjson sccache_resource_accounting_available "$sccache_resource_accounting_available" \
  --arg sccache_resource_accounting_reason "$sccache_resource_accounting_reason" \
  '{
    schema_version: 9,
    run_id: $run_id,
    evidence_kind: $evidence_kind,
    required_accepted_samples: $required_accepted_samples,
    attempts: $attempts,
    warmup_runs: $warmup_runs,
    workloads: $workloads,
    lanes: $lanes,
    fixture_source_sha256: $fixture_source_sha256,
    fixture_git_dependency_commit: $fixture_git_commit,
    fixture_lockfile_normalized_sha256: $fixture_lock_sha256,
    materialized_fixture_commit: $materialized_fixture_commit,
    materialized_fixture_tree: $materialized_fixture_tree,
    interleaving: "rotation-by-round",
    cargo_rail_native_cache: {
      available: true,
      platform: $cargo_rail_cache_platform,
      host_target: $cargo_rail_cache_host_target,
      execution_contract: $cargo_rail_cache_execution_contract,
      identity: $cargo_rail_cache_identity,
      reuse_scope: "physical-source-root",
      unavailable_reason: null
    },
    sccache: {
      available: $sccache_available,
      path: (if $sccache_path == "" then null else $sccache_path end),
      version: (if $sccache_version == "" then null else $sccache_version end),
      unavailable_reason: (if $sccache_unavailable_reason == "" then null else $sccache_unavailable_reason end),
      measured_modes: ["server_local_disk", "client_side_local_disk"],
      resource_accounting: {
        available: $sccache_resource_accounting_available,
        reason: (if $sccache_resource_accounting_reason == "" then null else $sccache_resource_accounting_reason end),
        cpu: "GNU time for the foreground server process and descendants plus the command process tree",
        peak_rss: "10 ms aggregate samples across the server and command process forests"
      }
    }
  }' >"$run_candidate"
if [[ "$operation" == resume ]]; then
  if ! jq -e -n --slurpfile recorded "$results/run.json" --slurpfile current "$run_candidate" \
    '$recorded[0] == $current[0]' >/dev/null; then
    echo "native-cache resume protocol changed; start a new result directory: $results" >&2
    jq -n --slurpfile recorded "$results/run.json" --slurpfile current "$run_candidate" \
      '{recorded: $recorded[0], current: $current[0]}' >&2
    exit 2
  fi
else
  mv "$run_candidate" "$results/run.json"
  mv "$native_cache_capability_candidate" "$results/native-cache-capability.json"
  mv "$native_cache_capability_stderr_candidate" "$results/native-cache-capability.stderr"
fi

workload_action() {
  if [[ "$1" == check ]]; then
    printf '%s\n' build
  else
    printf '%s\n' distribution
  fi
}

workload_cargo_subcommand() {
  if [[ "$1" == check ]]; then
    printf '%s\n' check
  else
    printf '%s\n' build
  fi
}

workload_profile_arguments() {
  if [[ "$1" == build ]]; then
    printf '%s\n' --release
  fi
}

shell_join() {
  local rendered="" argument quoted
  for argument in "$@"; do
    printf -v quoted '%q' "$argument"
    rendered="${rendered}${rendered:+ }${quoted}"
  done
  printf '%s\n' "$rendered"
}

argv_to_json() {
  printf '%s\n' "$@" | jq -Rsc 'split("\n")[:-1]'
}

stop_sccache() {
  local cache="$1"
  if [[ "$sccache_available" == true ]]; then
    env SCCACHE_DIR="$cache" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
  fi
}

sccache_cache_for_lane() {
  local workload="$1"
  local lane="$2"
  if [[ "$lane" == sccache-client-side ]]; then
    printf '%s\n' "$fixture_root/$workload-sccache-client-side-live-cache"
  else
    printf '%s\n' "$fixture_root/$workload-sccache-live-cache"
  fi
}

sccache_server_supervisor_pid=""
sccache_server_started_ns=""
sccache_server_ready_ns=""
sccache_server_timing=""
sccache_server_stdout=""
sccache_server_stderr=""
sccache_combined_rss=""
sccache_monitor_stop=""
sccache_monitor_pid=""

start_accounted_sccache_server() {
  local cache="$1"
  local lane_dir="$2"
  local ready=false
  local readiness="$lane_dir/sccache-server-readiness.json"

  sccache_server_timing="$lane_dir/sccache-server-time.json"
  sccache_server_stdout="$lane_dir/sccache-server.stdout"
  sccache_server_stderr="$lane_dir/sccache-server.stderr"
  sccache_combined_rss="$lane_dir/sccache-combined-rss.json"
  sccache_monitor_stop="$lane_dir/.sccache-monitor-stop"
  rm -f -- "$sccache_server_timing" "$sccache_server_stdout" "$sccache_server_stderr" \
    "$sccache_combined_rss" "$sccache_monitor_stop" "$readiness"

  sccache_server_started_ns="$(date +%s%N)"
  /usr/bin/time \
    -f '{"user_seconds":%U,"system_seconds":%S,"peak_rss_kib":%M,"exit_code":%x}' \
    -o "$sccache_server_timing" \
    env SCCACHE_DIR="$cache" SCCACHE_NO_DAEMON=1 SCCACHE_START_SERVER=1 "$sccache_bin" \
    >"$sccache_server_stdout" 2>"$sccache_server_stderr" &
  sccache_server_supervisor_pid=$!

  for _ in $(seq 1 300); do
    if env SCCACHE_DIR="$cache" "$sccache_bin" --show-stats --stats-format json >"$readiness" 2>/dev/null \
      && jq -e . "$readiness" >/dev/null 2>&1; then
      ready=true
      break
    fi
    if ! kill -0 "$sccache_server_supervisor_pid" 2>/dev/null; then
      break
    fi
    sleep 0.05
  done
  if [[ "$ready" != true ]]; then
    stop_sccache "$cache"
    wait "$sccache_server_supervisor_pid" 2>/dev/null || true
    echo "accounted sccache server failed to become ready" >&2
    sed -n '1,80p' "$sccache_server_stderr" >&2
    exit 1
  fi
  sccache_server_ready_ns="$(date +%s%N)"
}

run_hyperfine_with_sccache_rss() {
  local timing="$1"
  local output="$2"
  local command="$3"
  local hyperfine_status monitor_status

  set +e
  hyperfine --shell=bash --warmup 0 --runs 1 --ignore-failure --export-json "$timing" "$command" \
    >"$output" 2>&1 &
  local hyperfine_pid=$!
  python3 "$repo_root/scripts/bench/linux-process-forest-rss.py" \
    --output "$sccache_combined_rss" \
    --stop-file "$sccache_monitor_stop" \
    "$sccache_server_supervisor_pid" "$hyperfine_pid" &
  sccache_monitor_pid=$!
  wait "$hyperfine_pid"
  hyperfine_status=$?
  touch "$sccache_monitor_stop"
  wait "$sccache_monitor_pid"
  monitor_status=$?
  set -e
  if [[ "$monitor_status" -ne 0 ]]; then
    echo "sccache aggregate RSS monitor failed with status $monitor_status" >&2
    return 1
  fi
  return "$hyperfine_status"
}

finish_accounted_sccache_server() {
  local cache="$1"
  local timing="$2"
  local output="$3"
  local server_status=0
  local startup_seconds

  stop_sccache "$cache"
  set +e
  wait "$sccache_server_supervisor_pid"
  server_status=$?
  set -e
  startup_seconds="$(awk -v start="$sccache_server_started_ns" -v ready="$sccache_server_ready_ns" \
    'BEGIN { printf "%.9f", (ready - start) / 1000000000 }')"

  if [[ "$server_status" -eq 0 ]] \
    && jq -e '
      (.user_seconds | type) == "number"
      and (.system_seconds | type) == "number"
      and (.peak_rss_kib | type) == "number"
      and .exit_code == 0
    ' "$sccache_server_timing" >/dev/null \
    && jq -e '.available and .peak_rss_bytes > 0 and .samples > 0' "$sccache_combined_rss" >/dev/null; then
    jq -n \
      --argjson startup_seconds "$startup_seconds" \
      --slurpfile server "$sccache_server_timing" \
      --slurpfile combined "$sccache_combined_rss" \
      --slurpfile command "$timing" '
        ($command[0].results[0].memory_usage_byte[0] // 0) as $command_peak
        | ($server[0].peak_rss_kib * 1024) as $server_peak
        | {
            schema_version: 1,
            available: true,
            startup_seconds: $startup_seconds,
            user_seconds: $server[0].user_seconds,
            system_seconds: $server[0].system_seconds,
            cpu_seconds: ($server[0].user_seconds + $server[0].system_seconds),
            server_process_tree_peak_rss_bytes: $server_peak,
            sampled_combined_peak_rss_bytes: $combined[0].peak_rss_bytes,
            command_process_tree_peak_rss_bytes: $command_peak,
            peak_rss_bytes: ([$server_peak, $command_peak, $combined[0].peak_rss_bytes] | max),
            rss_sampling_interval_ms: $combined[0].interval_ms,
            rss_samples: $combined[0].samples,
            cpu_scope: "foreground_sccache_server_and_waited_compiler_descendants",
            peak_rss_scope: $combined[0].scope
          }
      ' >"$output"
  else
    jq -n \
      --argjson server_exit_code "$server_status" \
      '{
        schema_version: 1,
        available: false,
        reason: "foreground sccache server resource accounting failed",
        server_exit_code: $server_exit_code
      }' >"$output"
  fi

  sccache_server_supervisor_pid=""
  sccache_monitor_pid=""
}

seed_cargo_rail_cache() {
  local workload="$1"
  local root
  root="$(lane_root "$workload" cargo-rail-warm)"
  local cache="$fixture_root/$workload-cargo-rail-seed-cache"
  local status="$results/seed-$workload-cache-status.json"
  local stdout="$results/seed-$workload-cargo-rail.stdout"
  local stderr="$results/seed-$workload-cargo-rail.stderr"
  local action
  action="$(workload_action "$workload")"
  safe_remove "$root/target" "$cache"
  local args=(--all-features --locked --offline --message-format=json-render-diagnostics)
  if [[ "$workload" == build ]]; then
    args=(--all-features --offline --message-format=json-render-diagnostics)
  fi
  (
    cd "$root"
    env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      CARGO_RAIL_CACHE_DIR="$cache" \
      "$binary" rail run --all --action "$action" -- "${args[@]}" >"$stdout" 2>"$stderr"
    env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      CARGO_RAIL_CACHE_DIR="$cache" \
      "$binary" rail cache status --scope local -f json >"$status"
  )
  jq -e '.status.local.present and .status.local.cache.bytes > 0' "$status" >/dev/null
}

seed_sccache() {
  local workload="$1"
  local root="$fixture_root/$workload-sccache-seed"
  local cache="$fixture_root/$workload-sccache-seed-cache"
  local stdout="$results/seed-$workload-sccache.stdout"
  local stderr="$results/seed-$workload-sccache.stderr"
  safe_remove "$root/target" "$cache"
  if [[ "$sccache_available" != true ]]; then
    return
  fi
  stop_sccache "$cache"
  local roots
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) roots="$(cygpath -m "$root");$(cygpath -m "$fixture_root/$workload-sccache")" ;;
    *) roots="$root:$fixture_root/$workload-sccache" ;;
  esac
  local subcommand
  subcommand="$(workload_cargo_subcommand "$workload")"
  local args=(--workspace --all-features --locked --offline --message-format=json-render-diagnostics)
  if [[ "$workload" == build ]]; then
    args=(--workspace --release --all-features --locked --offline --message-format=json-render-diagnostics)
  fi
  (
    cd "$root"
    env -u RUSTC_WORKSPACE_WRAPPER -u SCCACHE_CLIENT_SIDE \
      SCCACHE_BASEDIRS="$roots" SCCACHE_DIR="$cache" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0 \
      cargo "$subcommand" "${args[@]}" >"$stdout" 2>"$stderr"
  )
  stop_sccache "$cache"
}

for workload in "${workloads[@]}"; do
  seed_cargo_rail_cache "$workload"
  seed_sccache "$workload"
done

seed_warm_target() {
  local workload="$1"
  local lane="$2"
  local incremental="$3"
  local root="$fixture_root/$workload-$lane"
  safe_remove "$root/target" "$fixture_root/$workload-$lane-cache"
  local subcommand
  subcommand="$(workload_cargo_subcommand "$workload")"
  local args=(--workspace --all-features --locked --offline --message-format=json-render-diagnostics)
  if [[ "$workload" == build ]]; then
    args=(--workspace --release --all-features --locked --offline --message-format=json-render-diagnostics)
  fi
  (
    cd "$root"
    if [[ "$incremental" == explicit ]]; then
      env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=1 \
        cargo "$subcommand" "${args[@]}" >/dev/null 2>&1
    else
      env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
        cargo "$subcommand" "${args[@]}" >/dev/null 2>&1
    fi
  )
}

for workload in "${workloads[@]}"; do
  seed_warm_target "$workload" native-cargo-warm manifest
  seed_warm_target "$workload" cargo-rail-delegated manifest
  seed_warm_target "$workload" native-cargo-incremental-warm explicit
  seed_warm_target "$workload" cargo-rail-incremental-bypass explicit
done

prepare_lane() {
  local workload="$1"
  local lane="$2"
  local root
  root="$(lane_root "$workload" "$lane")"
  case "$lane" in
    native-cargo-warm | native-cargo-incremental-warm | cargo-rail-delegated | cargo-rail-incremental-bypass) ;;
    *) safe_remove "$root/target" ;;
  esac
  case "$lane" in
    cargo-rail-cold)
      safe_remove "$fixture_root/$workload-cargo-rail-cold-cache"
      ;;
    cargo-rail-warm)
      safe_remove "$fixture_root/$workload-cargo-rail-warm-cache"
      cp -R "$fixture_root/$workload-cargo-rail-seed-cache" "$fixture_root/$workload-cargo-rail-warm-cache"
      ;;
    sccache | sccache-client-side | cargo-rail-sccache-preserved)
      if [[ "$sccache_available" == true ]]; then
        local live_cache
        live_cache="$(sccache_cache_for_lane "$workload" "$lane")"
        stop_sccache "$live_cache"
        safe_remove "$live_cache"
        cp -R "$fixture_root/$workload-sccache-seed-cache" "$live_cache"
      fi
      ;;
  esac
}

measured_command=""
measured_argv_json="[]"
measured_cache_state=""
command_for() {
  local workload="$1"
  local lane="$2"
  local diagnostics="$3"
  local stdout="$4"
  local stderr="$5"
  local evidence_mode="${6:-measured}"
  local root
  root="$(lane_root "$workload" "$lane")"
  local action subcommand
  action="$(workload_action "$workload")"
  subcommand="$(workload_cargo_subcommand "$workload")"
  local cargo_args=(--workspace --all-features --locked --offline --message-format=json-render-diagnostics)
  local rail_args=(--all-features --locked --offline --message-format=json-render-diagnostics)
  if [[ "$workload" == build ]]; then
    cargo_args=(--workspace --release --all-features --locked --offline --message-format=json-render-diagnostics)
    rail_args=(--all-features --offline --message-format=json-render-diagnostics)
  fi
  local argv environment_prefix
  local rail_diagnostics_args=()
  local rail_evidence_args=()
  if [[ "$evidence_mode" == explain ]]; then
    rail_diagnostics_args=(--diagnostics-file "$diagnostics")
    rail_evidence_args=(--explain)
  fi
  case "$lane" in
    native-cargo)
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      measured_cache_state="native-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0)"
      ;;
    native-cargo-warm)
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      measured_cache_state="native-retained-warm-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER)"
      ;;
    native-cargo-incremental-warm)
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      measured_cache_state="native-explicit-incremental-retained-warm-target"
      environment_prefix="$(shell_join env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=1)"
      ;;
    cargo-rail-delegated)
      local delegated_cache="$fixture_root/$workload-cargo-rail-delegated-cache"
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="active-profile-delegation-retained-warm-target-empty-cache"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$delegated_cache")"
      ;;
    cargo-rail-incremental-bypass)
      local bypass_cache="$fixture_root/$workload-cargo-rail-incremental-bypass-cache"
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="explicit-incremental-bypass-retained-warm-target-empty-cache"
      environment_prefix="$(shell_join env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=1 CARGO_RAIL_CACHE_DIR="$bypass_cache")"
      ;;
    cargo-rail-disabled)
      local disabled_cache="$fixture_root/$workload-cargo-rail-disabled-cache"
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" --no-cache "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" --no-cache "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="disabled-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$disabled_cache")"
      ;;
    cargo-rail-cold)
      local cold_cache="$fixture_root/$workload-cargo-rail-cold-cache"
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="empty-cache-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$cold_cache")"
      ;;
    cargo-rail-warm)
      local warm_cache="$fixture_root/$workload-cargo-rail-warm-cache"
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="verified-same-root-seed-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$warm_cache")"
      ;;
    sccache | sccache-client-side)
      local sccache_cache
      sccache_cache="$(sccache_cache_for_lane "$workload" "$lane")"
      local basedirs
      case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*)
          basedirs="$(cygpath -m "$fixture_root/$workload-sccache-seed");$(cygpath -m "$root")"
          ;;
        *) basedirs="$fixture_root/$workload-sccache-seed:$root" ;;
      esac
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      local sccache_mode="server"
      local sccache_mode_environment=(-u SCCACHE_CLIENT_SIDE)
      if [[ "$lane" == sccache-client-side ]]; then
        sccache_mode="client-side"
        sccache_mode_environment+=(SCCACHE_CLIENT_SIDE=1)
      fi
      measured_cache_state="sccache-$sccache_expected_version-$sccache_mode-cross-root-seed-empty-target"
      environment_prefix="$(shell_join env -u RUSTC_WORKSPACE_WRAPPER "${sccache_mode_environment[@]}" SCCACHE_BASEDIRS="$basedirs" SCCACHE_DIR="$sccache_cache" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0)"
      ;;
    cargo-rail-sccache-preserved)
      local preserved_sccache_cache="$fixture_root/$workload-sccache-live-cache"
      local preserved_rail_cache="$fixture_root/$workload-cargo-rail-sccache-preserved-cache"
      local preserved_basedirs
      case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*)
          preserved_basedirs="$(cygpath -m "$fixture_root/$workload-sccache-seed");$(cygpath -m "$root")"
          ;;
        *) preserved_basedirs="$fixture_root/$workload-sccache-seed:$root" ;;
      esac
      argv="$(shell_join "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail "${rail_diagnostics_args[@]}" run --all --action "$action" "${rail_evidence_args[@]}" -- "${rail_args[@]}")"
      measured_cache_state="sccache-$sccache_expected_version-preserved-cross-root-seed-empty-target-empty-cargo-rail-cache"
      environment_prefix="$(shell_join env -u RUSTC_WORKSPACE_WRAPPER -u SCCACHE_CLIENT_SIDE SCCACHE_BASEDIRS="$preserved_basedirs" SCCACHE_DIR="$preserved_sccache_cache" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR="$preserved_rail_cache")"
      ;;
  esac
  # Hyperfine and its nested Bash inject process controls into each command.
  # They are benchmark machinery, not compiler inputs, so remove them before
  # exact environment capture just as we remove wrapper controls above.
  measured_command="cd $(shell_join "$root") && env -u HYPERFINE_ITERATION -u HYPERFINE_RANDOMIZED_ENVIRONMENT_OFFSET -u SHLVL -u __CF_USER_TEXT_ENCODING $environment_prefix $argv >$(shell_join "$stdout") 2>$(shell_join "$stderr")"
}

output_manifest() {
  local root="$1"
  local output="$2"
  local target="$root/target"
  local entries="${output}.entries"
  : >"$entries"
  if [[ -d "$target" ]]; then
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      local relative="${path#"$target"/}"
      local bytes digest
      bytes="$(wc -c <"$path" | tr -d ' ')"
      digest="$(sha256_file "$path")"
      jq -nc --arg path "$relative" --argjson bytes "$bytes" --arg sha256 "$digest" \
        '{path: $path, bytes: $bytes, sha256: $sha256}' >>"$entries"
    done < <(
      find "$target" -type f \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -print | LC_ALL=C sort
    )
  fi
  local entries_json="${output}.entries.json"
  jq -sc 'sort_by(.path)' "$entries" >"$entries_json"
  local digest
  digest="$(sha256_file "$entries_json")"
  jq -n \
    --arg digest "sha256:$digest" \
    --slurpfile entries "$entries_json" \
    '{
      schema_version: 2,
      digest: $digest,
      entries: $entries[0],
      entry_count: ($entries[0] | length),
      available: (($entries[0] | length) > 0)
    }' >"$output"
  rm -f -- "$entries" "$entries_json"
}

action_census() {
  local stdout="$1"
  local output="$2"
  local entries="${output}.entries"
  jq -Rsc '
    [
      split("\n")[]
      | fromjson?
      | select(.reason == "compiler-artifact")
      | {
          package_id: (
            .package_id
            | sub("^path\\+file://[^#]+#"; "path#")
            | sub("^git\\+file://[^?#]+"; "git+file")
          ),
          target: {
            name: .target.name,
            kind: (.target.kind | sort),
            crate_types: (.target.crate_types | sort)
          },
          profile: {
            opt_level: .profile.opt_level,
            debuginfo: .profile.debuginfo,
            debug_assertions: .profile.debug_assertions,
            overflow_checks: .profile.overflow_checks,
            test: .profile.test
          },
          features: (.features | sort),
          filenames: (
            (.filenames // [])
            | map(sub("^.*[/\\\\]target[/\\\\]"; "target/") | gsub("\\\\"; "/"))
            | sort
          )
        }
    ]
    | sort_by(tojson)
  ' "$stdout" >"$entries"
  local digest count
  digest="$(sha256_file "$entries")"
  count="$(jq 'length' "$entries")"
  jq -n --arg digest "sha256:$digest" --argjson count "$count" --slurpfile entries "$entries" \
    '{schema_version: 1, digest: $digest, count: $count, available: ($count > 0), units: $entries[0]}' >"$output"
  rm -f -- "$entries"
}

native_cache_report() {
  local stdout="$1"
  local stderr="$2"
  local output="$3"
  local events="${output}.events"
  grep 'native compiler cache event:' "$stdout" "$stderr" \
    | sed 's/^.* native compiler cache event: //' \
    | jq -s 'sort_by([.unit_identity, .result_key, .outcome, .reason])' >"$events" || printf '%s\n' '[]' >"$events"
  local eligible="${output}.eligible"
  jq '[.[] | select((.outcome == "hit" or .outcome == "miss") and .unit_identity != null) | .unit_identity] | sort' \
    "$events" >"$eligible"
  local event_digest eligible_digest
  event_digest="$(sha256_file "$events")"
  eligible_digest="$(sha256_file "$eligible")"
  local line
  line="$(grep 'native compiler cache:' "$stdout" "$stderr" | tail -1 || true)"
  if [[ "$line" == *"native compiler cache: bypassed ("* || "$line" == *"native compiler cache: bypassed reason="* ]]; then
    local reason
    if [[ "$line" == *"native compiler cache: bypassed ("* ]]; then
      reason="${line##*native compiler cache: bypassed (}"
      reason="${reason%)}"
    else
      reason="${line##*native compiler cache: bypassed reason=}"
    fi
    jq -n \
      --arg reason "$reason" \
      --arg event_digest "sha256:$event_digest" \
      --arg eligible_digest "sha256:$eligible_digest" \
      --slurpfile events "$events" \
      --slurpfile eligible "$eligible" \
      '{
        available: true,
        kind: "cargo-rail-native",
        status: "bypassed",
        hits: 0,
        misses: 0,
        bypasses: 0,
        unit_invocations: 0,
        setup_bytes_hashed: 0,
        unit_bytes_hashed: 0,
        cache_bytes_read: 0,
        cache_bytes_written: 0,
        bytes_restored: 0,
        reason: $reason,
        event_identity_digest: $event_digest,
        eligible_unit_digest: $eligible_digest,
        outcome_digest: ("action-bypass:" + $reason),
        events: $events[0],
        eligible_units: $eligible[0]
      }' \
      >"$output"
    return
  fi
  if [[ -z "$line" ]]; then
    jq -n \
      --arg event_digest "sha256:$event_digest" \
      --arg eligible_digest "sha256:$eligible_digest" \
      --slurpfile events "$events" \
      --slurpfile eligible "$eligible" \
      '{
        available: false,
        kind: "cargo-rail-native",
        status: "missing",
        event_identity_digest: $event_digest,
        eligible_unit_digest: $eligible_digest,
        outcome_digest: "missing",
        events: $events[0],
        eligible_units: $eligible[0]
      }' >"$output"
    return
  fi
  metric() {
    local name="$1"
    sed -n "s/.*[[:space:]]$name=\\([0-9][0-9]*\\).*/\\1/p" <<<"$line"
  }
  local reasons="${line#* reasons=}"
  jq -n \
    --argjson hits "$(metric hits)" \
    --argjson misses "$(metric misses)" \
    --argjson bypasses "$(metric bypasses)" \
    --argjson setup_bytes_hashed "$(metric setup_bytes_hashed)" \
    --argjson unit_bytes_hashed "$(metric bytes_hashed)" \
    --argjson cache_bytes_read "$(metric cache_bytes_read)" \
    --argjson cache_bytes_written "$(metric cache_bytes_written)" \
    --argjson bytes_restored "$(metric bytes_restored)" \
    --arg reasons "$reasons" \
    --arg event_digest "sha256:$event_digest" \
    --arg eligible_digest "sha256:$eligible_digest" \
    --slurpfile events "$events" \
    --slurpfile eligible "$eligible" \
    '{
      available: true,
      kind: "cargo-rail-native",
      status: "reported",
      hits: $hits,
      misses: $misses,
      bypasses: $bypasses,
      unit_invocations: ($hits + $misses + $bypasses),
      setup_bytes_hashed: $setup_bytes_hashed,
      unit_bytes_hashed: $unit_bytes_hashed,
      cache_bytes_read: $cache_bytes_read,
      cache_bytes_written: $cache_bytes_written,
      bytes_restored: $bytes_restored,
      reasons: $reasons,
      event_identity_digest: $event_digest,
      eligible_unit_digest: $eligible_digest,
      outcome_digest: $event_digest,
      events: $events[0],
      eligible_units: $eligible[0]
  }' >"$output"
}

native_cache_measured_outcome() {
  local stdout="$1"
  local stderr="$2"
  local output="$3"
  local line
  line="$(grep 'native compiler cache:' "$stdout" "$stderr" | tail -1 || true)"
  if [[ "$line" == *"native compiler cache: bypassed ("* || "$line" == *"native compiler cache: bypassed reason="* ]]; then
    local reason
    if [[ "$line" == *"native compiler cache: bypassed ("* ]]; then
      reason="${line##*native compiler cache: bypassed (}"
      reason="${reason%)}"
    else
      reason="${line##*native compiler cache: bypassed reason=}"
    fi
    jq -n --arg reason "$reason" \
      '{available:true,status:"bypassed",reason:$reason,hits:0,misses:0,bypasses:0,bytes_restored:0}' >"$output"
    return
  fi
  if [[ -z "$line" ]]; then
    printf '%s\n' '{"available":false,"status":"missing"}' >"$output"
    return
  fi
  metric() {
    local name="$1"
    sed -n "s/.*[[:space:]]$name=\\([0-9][0-9]*\\).*/\\1/p" <<<"$line"
  }
  jq -n \
    --argjson hits "$(metric hits)" \
    --argjson misses "$(metric misses)" \
    --argjson bypasses "$(metric bypasses)" \
    --argjson bytes_restored "$(metric bytes_restored)" \
    '{
      available:true,
      status:"reported",
      hits:$hits,
      misses:$misses,
      bypasses:$bypasses,
      bytes_restored:$bytes_restored
    }' >"$output"
}

sccache_report() {
  local cache="$1"
  local output="$2"
  if [[ "$sccache_available" != true ]]; then
    jq -n --arg reason "$sccache_unavailable_reason" --arg version "$sccache_expected_version" \
      '{available: false, kind: ("sccache-" + $version), status: "unavailable", reason: $reason}' >"$output"
    return
  fi
  env SCCACHE_DIR="$cache" "$sccache_bin" --show-stats --stats-format json >"${output}.raw" 2>/dev/null || true
  if ! jq -e . "${output}.raw" >/dev/null 2>&1; then
    jq -n --arg version "$sccache_expected_version" \
      '{available: false, kind: ("sccache-" + $version), status: "stats-unavailable"}' >"$output"
    return
  fi
  jq --arg version "$sccache_expected_version" '{
    available: true,
    kind: ("sccache-" + $version),
    status: "reported",
    compile_requests: .stats.compile_requests,
    hits: ([.stats.cache_hits.counts[]] | add // 0),
    misses: ([.stats.cache_misses.counts[]] | add // 0),
    bypasses: .stats.non_cacheable_compilations,
    not_cached: .stats.not_cached,
    raw: .
  }' "${output}.raw" >"$output"
  local outcome="${output}.outcome"
  jq -S '{hits, misses, bypasses}' "$output" >"$outcome"
  local outcome_digest
  outcome_digest="$(sha256_file "$outcome")"
  jq --arg outcome_digest "sha256:$outcome_digest" \
    '. + {
      event_identity_digest: null,
      eligible_unit_digest: null,
      outcome_digest: $outcome_digest,
      events: null,
      eligible_units: null
    }' "$output" >"${output}.updated"
  mv "${output}.updated" "$output"
}

remote_cache_report() {
  local stdout="$1"
  local stderr="$2"
  local output="$3"
  local line
  line="$(grep 'remote compiler cache:' "$stdout" "$stderr" | tail -1 || true)"
  if [[ -z "$line" ]]; then
    printf '%s\n' '{"available":false,"status":"missing"}' >"$output"
    return
  fi
  metric() {
    local name="$1"
    sed -n "s/.*[[:space:]]$name=\\([0-9][0-9]*\\).*/\\1/p" <<<"$line"
  }
  jq -n \
    --argjson requests "$(metric requests)" \
    --argjson bytes "$(metric bytes)" \
    --argjson hits "$(metric hits)" \
    --argjson misses "$(metric misses)" \
    --argjson conflicts "$(metric conflicts)" \
    --argjson failures "$(metric failures)" \
    --argjson publications "$(metric publications)" \
    '{
      available: true,
      status: "reported",
      requests: $requests,
      payload_bytes: $bytes,
      hits: $hits,
      misses: $misses,
      conflicts: $conflicts,
      failures: $failures,
      publications: $publications
    }' >"$output"
}

tree_file_bytes() {
  local root="$1"
  local total=0 path bytes
  if [[ -d "$root" ]]; then
    while IFS= read -r -d '' path; do
      bytes="$(wc -c <"$path" | tr -d ' ')"
      total=$((total + bytes))
    done < <(find "$root" -type f -print0)
  fi
  printf '%s\n' "$total"
}

l2_storage_inventory() {
  local label="$1"
  local output="$2"
  if ! command -v aws >/dev/null; then
    jq -n --arg label "$label" '{
      schema_version: 1,
      label: $label,
      available: false,
      reason: "AWS CLI unavailable; retain exact namespace inventory from the operator",
      object_count: null,
      bytes: null,
      protocol_markers: null,
      selectors: null,
      actions: null,
      results: null,
      identity: null
    }' >"$output"
    return
  fi
  local region owner bucket prefix namespace raw
  IFS=$'\t' read -r region owner bucket prefix < <(
    jq -er --arg alias "$l2_alias" '
      .targets[$alias] | [.region, .expected_bucket_owner, .bucket, .prefix] | @tsv
    ' "$l2_targets_file"
  )
  namespace="${prefix:+$prefix/}native-v3/"
  raw="${output}.raw"
  aws s3api list-objects-v2 \
    --region "$region" \
    --bucket "$bucket" \
    --expected-bucket-owner "$owner" \
    --prefix "$namespace" \
    --no-cli-pager \
    --output json >"$raw"
  jq --arg label "$label" --arg namespace "$namespace" '
    [(.Contents // [])[] | {path: (.Key | ltrimstr($namespace)), bytes: .Size}]
    | sort_by(.path) as $objects
    | {
        schema_version: 1,
        label: $label,
        available: true,
        object_count: ($objects | length),
        bytes: ($objects | map(.bytes) | add // 0),
        protocol_markers: ($objects | map(select(.path == "protocol")) | length),
        selectors: ($objects | map(select(.path | startswith("selectors/"))) | length),
        actions: ($objects | map(select(.path | startswith("actions/"))) | length),
        results: ($objects | map(select(.path | startswith("results/"))) | length),
        identity: ($objects | map([.path, .bytes]))
      }
  ' "$raw" >"$output"
  rm -f -- "$raw"
}

runtime_report() {
  local workload="$1"
  local root="$2"
  local output="$3"
  if [[ "$workload" != build ]]; then
    printf '%s\n' '{"available":false,"reason":"check workload does not link the fixture executable"}' >"$output"
    return
  fi
  local executable="$root/target/release/fixture-cli"
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) executable="${executable}.exe" ;;
  esac
  if [[ ! -f "$executable" || ! -x "$executable" ]]; then
    printf '%s\n' '{"available":false,"reason":"release fixture executable is unavailable"}' >"$output"
    return
  fi
  local runtime_stdout="${output}.stdout"
  local runtime_stderr="${output}.stderr"
  set +e
  "$executable" >"$runtime_stdout" 2>"$runtime_stderr"
  local exit_code=$?
  set -e
  local stdout_sha256 stderr_sha256
  stdout_sha256="$(sha256_file "$runtime_stdout")"
  stderr_sha256="$(sha256_file "$runtime_stderr")"
  jq -n \
    --argjson exit_code "$exit_code" \
    --arg stdout_sha256 "sha256:$stdout_sha256" \
    --arg stderr_sha256 "sha256:$stderr_sha256" \
    --argjson stdout_bytes "$(wc -c <"$runtime_stdout" | tr -d ' ')" \
    --argjson stderr_bytes "$(wc -c <"$runtime_stderr" | tr -d ' ')" \
    '{
      available: true,
      exit_code: $exit_code,
      stdout_sha256: $stdout_sha256,
      stderr_sha256: $stderr_sha256,
      stdout_bytes: $stdout_bytes,
      stderr_bytes: $stderr_bytes,
      identity: [$exit_code, $stdout_sha256, $stderr_sha256, $stdout_bytes, $stderr_bytes]
    }' >"$output"
}

measure_l2_lane() {
  local lane="$1"
  local l2_root="$2"
  local l2_results="$3"
  local toolchain="$4"
  local lane_dir="$l2_results/$lane"
  local l1="$fixture_root/l2-$lane-cache"
  local stdout="$lane_dir/stdout.txt"
  local stderr="$lane_dir/stderr.txt"
  local diagnostics="$lane_dir/diagnostics.json"
  local timing="$lane_dir/timing.json"
  local native="$lane_dir/native-cache.json"
  local remote="$lane_dir/remote-cache.json"
  local storage="$lane_dir/storage.json"
  local output_path output_sha256 output_bytes exit_code diagnostics_valid=false
  local environment_prefix command hyperfine_status
  local -a environment=(
    env -i
    "HOME=$HOME"
    "PATH=$PATH"
    "LANG=C.UTF-8"
    "RUSTUP_TOOLCHAIN=$toolchain"
    "CARGO_INCREMENTAL=0"
    "CARGO_RAIL_CACHE_TARGETS_FILE=$l2_targets_file"
    "CARGO_RAIL_CACHE_DIR=$l1"
  )
  local -a arguments=(
    --locked
    --offline
    --message-format=json-render-diagnostics
    --no-default-features
    --exclude fixture-api
    --exclude fixture-build
    --exclude fixture-cli
    --exclude fixture-macros
    --exclude fixture-native
    --exclude fixture-native-sys
    --exclude fixture-service-a
    --exclude fixture-service-b
    --exclude fixture-storage
  )

  mkdir -p "$lane_dir"
  safe_remove "$l2_root/target" "$l1"
  if [[ "$lane" == unavailable ]]; then
    environment+=(
      "AWS_EC2_METADATA_DISABLED=true"
      "AWS_CONFIG_FILE=/dev/null"
      "AWS_SHARED_CREDENTIALS_FILE=/dev/null"
    )
  else
    local name
    for name in \
      AWS_REGION \
      AWS_DEFAULT_REGION \
      AWS_PROFILE \
      AWS_DEFAULT_PROFILE \
      AWS_CONFIG_FILE \
      AWS_SHARED_CREDENTIALS_FILE \
      AWS_SDK_LOAD_CONFIG \
      AWS_ROLE_ARN \
      AWS_WEB_IDENTITY_TOKEN_FILE \
      AWS_CONTAINER_CREDENTIALS_RELATIVE_URI \
      AWS_CONTAINER_CREDENTIALS_FULL_URI \
      AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE
    do
      if [[ -v "$name" ]]; then
        environment+=("$name=${!name}")
      fi
    done
  fi

  environment_prefix="$(shell_join "${environment[@]}")"
  command="cd $(shell_join "$l2_root") && $environment_prefix $(shell_join \
    "$binary" rail --diagnostics-file "$diagnostics" run --all --action build --explain -- "${arguments[@]}") \
    >$(shell_join "$stdout") 2>$(shell_join "$stderr")"
  set +e
  hyperfine --shell=bash --warmup 0 --runs 1 --ignore-failure --export-json "$timing" "$command" \
    >"$lane_dir/hyperfine.txt" 2>&1
  hyperfine_status=$?
  set -e
  if [[ "$hyperfine_status" -ne 0 || ! -s "$timing" ]]; then
    jq -n --argjson status "$hyperfine_status" \
      '{results:[{times:[],user:null,system:null,memory_usage_byte:[],exit_codes:[]}],hyperfine_exit_code:$status}' \
      >"$timing"
  fi

  native_cache_report "$stdout" "$stderr" "$native"
  remote_cache_report "$stdout" "$stderr" "$remote"
  l2_storage_inventory "after-$lane" "$storage"
  exit_code="$(jq '.results[0].exit_codes[0] // null' "$timing")"
  if [[ -s "$diagnostics" ]] && jq -e '.schema_version == 10 and (.phases | length == 7)' "$diagnostics" >/dev/null; then
    diagnostics_valid=true
  fi
  output_path="$(find "$l2_root/target" -type f -name 'libfixture_types-*.rmeta' -print)"
  if [[ -f "$output_path" && "$(printf '%s\n' "$output_path" | wc -l | tr -d ' ')" -eq 1 ]]; then
    output_sha256="sha256:$(sha256_file "$output_path")"
    output_bytes="$(wc -c <"$output_path" | tr -d ' ')"
  else
    output_sha256=""
    output_bytes=0
  fi

  jq -n \
    --arg lane "$lane" \
    --argjson exit_code "$exit_code" \
    --argjson diagnostics_valid "$diagnostics_valid" \
    --arg output_sha256 "$output_sha256" \
    --argjson output_bytes "$output_bytes" \
    --argjson target_bytes "$(tree_file_bytes "$l2_root/target")" \
    --argjson l1_bytes "$(tree_file_bytes "$l1")" \
    --argjson exact_argv "$(argv_to_json \
      "$binary" rail --diagnostics-file '<result>/diagnostics.json' run --all --action build --explain -- \
      "${arguments[@]}")" \
    --slurpfile timing "$timing" \
    --slurpfile native "$native" \
    --slurpfile remote "$remote" \
    --slurpfile storage "$storage" \
    '{
      schema_version: 1,
      lane: $lane,
      exit_code: $exit_code,
      diagnostics_valid: $diagnostics_valid,
      exact_argv: $exact_argv,
      resources: {
        wall_seconds: ($timing[0].results[0].times[0] // null),
        user_seconds: ($timing[0].results[0].user // null),
        system_seconds: ($timing[0].results[0].system // null),
        cpu_seconds: (
          if $timing[0].results[0].user == null or $timing[0].results[0].system == null then null
          else $timing[0].results[0].user + $timing[0].results[0].system
          end
        ),
        peak_rss_bytes: ($timing[0].results[0].memory_usage_byte[0] // null),
        target_file_bytes: $target_bytes,
        l1_file_bytes: $l1_bytes,
        process_count: null,
        process_count_unavailable_reason: "the one-shot command path does not inject a process-count sampler"
      },
      native: $native[0],
      remote: $remote[0],
      storage: $storage[0],
      output: {
        available: ($output_sha256 != ""),
        sha256: (if $output_sha256 == "" then null else $output_sha256 end),
        bytes: $output_bytes
      }
    }' >"$lane_dir/result.json"
}

measure_l2_cycle() {
  local l2_results="$results/l2"
  if [[ -f "$l2_results/summary.json" ]] && jq -e '.accepted' "$l2_results/summary.json" >/dev/null; then
    echo "native-cache resume: keeping complete L2 cycle" >&2
    return
  fi
  [[ ! -e "$l2_results" ]] || {
    echo "native-cache L2 result is incomplete; start a new result directory: $l2_results" >&2
    exit 2
  }
  local l2_root="$fixture_root/l2-workspace"
  local toolchain
  toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$repo_root/rust-toolchain.toml" | head -1)"
  [[ -n "$toolchain" ]] || {
    echo "native-cache L2 measurement cannot resolve the pinned Rust toolchain" >&2
    exit 2
  }
  mkdir -p "$l2_results"
  materialize l2-workspace
  mkdir -p "$l2_root/.config"
  printf '[cache]\nl2 = "%s"\n' "$l2_alias" >"$l2_root/.config/rail.toml"
  l2_storage_inventory initial "$l2_results/storage-initial.json"
  measure_l2_lane publish "$l2_root" "$l2_results" "$toolchain"
  measure_l2_lane hit "$l2_root" "$l2_results" "$toolchain"
  measure_l2_lane unavailable "$l2_root" "$l2_results" "$toolchain"

  jq -n \
    --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg target_map_sha256 "sha256:$l2_targets_sha256" \
    --arg binary_sha256 "sha256:$binary_sha256" \
    --argjson binary_bytes "$(wc -c <"$binary" | tr -d ' ')" \
    --slurpfile initial "$l2_results/storage-initial.json" \
    --slurpfile publish "$l2_results/publish/result.json" \
    --slurpfile hit "$l2_results/hit/result.json" \
    --slurpfile unavailable "$l2_results/unavailable/result.json" \
    '
      $publish[0] as $p
      | $hit[0] as $h
      | $unavailable[0] as $u
      | {
          schema_version: 1,
          generated_at: $generated_at,
          sampling: "single retained operational observation per lane; no warmup and no percentile claim",
          workload: "one clean eligible fixture library at one canonical physical source root",
          identity: {
            target_map_sha256: $target_map_sha256,
            cargo_rail_binary_sha256: $binary_sha256,
            cargo_rail_binary_bytes: $binary_bytes
          },
          accounting: {
            requests: "authenticated compiler-wrapper coordinator requests, not raw S3 API attempts",
            network_bytes: "verified result-pack payload bytes transferred, excluding protocol and transport overhead",
            storage_cost_input: "exact S3 object bytes when an inventory client is present; otherwise operator-retained",
            resource_scope: "whole command process tree as reported by hyperfine"
          },
          storage_initial: $initial[0],
          lanes: {publish: $p, hit: $h, unavailable: $u},
          output_identity_preserved: (
            $p.output.available and $p.output.sha256 == $h.output.sha256 and $p.output.sha256 == $u.output.sha256
          ),
          action_identity_preserved: (
            ($p.native.eligible_units | length) == 1
            and $p.native.eligible_units == $h.native.eligible_units
            and $p.native.eligible_units == $u.native.eligible_units
          ),
          storage_stable_after_publication: (
            if $initial[0].available then
              $initial[0].object_count == 1
              and $p.storage.protocol_markers == 1
              and $p.storage.selectors == 1
              and $p.storage.actions == 1
              and $p.storage.results == 1
              and $p.storage.identity == $h.storage.identity
              and $p.storage.identity == $u.storage.identity
            else null
            end
          )
        }
      | .accepted = (
          .output_identity_preserved
          and .action_identity_preserved
          and (.storage_stable_after_publication != false)
          and ([.lanes[]] | all(
            .exit_code == 0
            and .diagnostics_valid
            and .resources.wall_seconds != null
            and .resources.cpu_seconds != null
            and .resources.peak_rss_bytes != null
          ))
          and $p.native.hits == 0 and $p.native.misses == 1
          and $p.remote.hits == 0 and $p.remote.misses == 1
          and $p.remote.conflicts == 0 and $p.remote.failures == 0 and $p.remote.publications == 1
          and $h.native.hits == 1 and $h.native.misses == 0
          and $h.remote.hits == 2 and $h.remote.misses == 0
          and $h.remote.conflicts == 0 and $h.remote.failures == 0 and $h.remote.publications == 0
          and $u.native.hits == 0 and $u.native.misses == 1
          and $u.remote.hits == 0 and $u.remote.failures >= 1 and $u.remote.publications == 0
        )
    ' >"$l2_results/summary.json"
  if ! jq -e '.accepted' "$l2_results/summary.json" >/dev/null; then
    echo "native-cache L2 operational cycle failed validation" >&2
    jq '{accepted, output_identity_preserved, action_identity_preserved, storage_stable_after_publication, lanes}' \
      "$l2_results/summary.json" >&2
    return 1
  fi
}

measure_lane() {
  local round="$1"
  local workload="$2"
  local lane="$3"
  local position="$4"
  local group_dir="$5"
  local lane_dir="$group_dir/$lane"
  mkdir -p "$lane_dir"
  local diagnostics="$lane_dir/diagnostics.json"
  local stdout="$lane_dir/stdout.txt"
  local stderr="$lane_dir/stderr.txt"
  local timing="$lane_dir/timing.json"
  local manifest="$lane_dir/output-manifest.json"
  local census="$lane_dir/action-census.json"
  local cache_report="$lane_dir/cache-report.json"
  local runtime="$lane_dir/runtime.json"
  local measured_outcome="$lane_dir/measured-cache-outcome.json"
  local specialist_resources="$lane_dir/sccache-resources.json"
  local available=true
  local unavailable_reason=""
  local sample_argv_json="[]"
  local sample_cache_state="unavailable"
  local sample_command=""
  jq -n \
    --arg reason "lane does not use an accounted detached specialist server" \
    '{schema_version: 1, available: false, reason: $reason}' >"$specialist_resources"
  if [[ ("$lane" == sccache || "$lane" == sccache-client-side || "$lane" == cargo-rail-sccache-preserved) \
    && "$sccache_available" != true ]]; then
    available=false
    unavailable_reason="$sccache_unavailable_reason"
  fi
  prepare_lane "$workload" "$lane"

  if [[ "$available" != true ]]; then
    jq -n --arg reason "$unavailable_reason" \
      '{results:[{times:[],user:null,system:null,memory_usage_byte:[],exit_codes:[]}], unavailable_reason:$reason}' \
      >"$timing"
    : >"$stdout"
    : >"$stderr"
    measured_argv_json="[]"
    measured_cache_state="unavailable"
    measured_command=""
  else
    command_for "$workload" "$lane" "$diagnostics" "$stdout" "$stderr"
    sample_argv_json="$measured_argv_json"
    sample_cache_state="$measured_cache_state"
    sample_command="$measured_command"
    local accounted_sccache=false
    if [[ "$lane" == sccache || "$lane" == sccache-client-side || "$lane" == cargo-rail-sccache-preserved ]]; then
      if [[ "$sccache_resource_accounting_available" == true ]]; then
        start_accounted_sccache_server "$(sccache_cache_for_lane "$workload" "$lane")" "$lane_dir"
        accounted_sccache=true
      else
        jq -n --arg reason "$sccache_resource_accounting_reason" \
          '{schema_version: 1, available: false, reason: $reason}' >"$specialist_resources"
      fi
    fi
    set +e
    if [[ "$accounted_sccache" == true ]]; then
      run_hyperfine_with_sccache_rss "$timing" "$lane_dir/hyperfine.txt" "$measured_command"
    else
      hyperfine --shell=bash --warmup 0 --runs 1 --ignore-failure --export-json "$timing" "$measured_command" \
        >"$lane_dir/hyperfine.txt" 2>&1
    fi
    local hyperfine_status=$?
    set -e
    if [[ "$hyperfine_status" -ne 0 || ! -s "$timing" ]]; then
      jq -n --argjson status "$hyperfine_status" \
        '{results:[{times:[],user:null,system:null,memory_usage_byte:[],exit_codes:[]}], hyperfine_exit_code:$status}' \
        >"$timing"
    fi
  fi

  local root
  root="$(lane_root "$workload" "$lane")"
  output_manifest "$root" "$manifest"
  action_census "$stdout" "$census"
  runtime_report "$workload" "$root" "$runtime"
  local diagnostics_evidence="$diagnostics"
  local diagnostics_source="not_applicable"
  local diagnostics_replay_exit_code=null
  case "$lane" in
    cargo-rail-cold | cargo-rail-warm)
      native_cache_measured_outcome "$stdout" "$stderr" "$measured_outcome"
      if [[ "$available" == true ]]; then
        local proof_dir="$lane_dir/proof-replay"
        local proof_diagnostics="$proof_dir/diagnostics.json"
        local proof_stdout="$proof_dir/stdout.txt"
        local proof_stderr="$proof_dir/stderr.txt"
        local proof_manifest="$proof_dir/output-manifest.json"
        local proof_census="$proof_dir/action-census.json"
        mkdir -p "$proof_dir"
        prepare_lane "$workload" "$lane"
        command_for "$workload" "$lane" "$proof_diagnostics" "$proof_stdout" "$proof_stderr" explain
        local proof_argv_json="$measured_argv_json"
        set +e
        bash -c "$measured_command"
        local proof_exit_code=$?
        set -e
        output_manifest "$root" "$proof_manifest"
        action_census "$proof_stdout" "$proof_census"
        native_cache_report "$proof_stdout" "$proof_stderr" "$cache_report"
        local proof_diagnostics_valid=false
        if [[ "$proof_exit_code" -eq 0 && -s "$proof_diagnostics" ]] \
          && jq -e '.schema_version == 10 and (.phases | length == 7)' "$proof_diagnostics" >/dev/null; then
          proof_diagnostics_valid=true
        fi
        local proof_outputs_identical=false
        local proof_census_identical=false
        local proof_outcome_identical=false
        if [[ "$(jq -r '.digest' "$manifest")" == "$(jq -r '.digest' "$proof_manifest")" ]]; then
          proof_outputs_identical=true
        fi
        if [[ "$(jq -r '.digest' "$census")" == "$(jq -r '.digest' "$proof_census")" ]]; then
          proof_census_identical=true
        fi
        if jq -e -n --slurpfile measured "$measured_outcome" --slurpfile proof "$cache_report" '
          $measured[0].available
          and $measured[0].status == $proof[0].status
          and ($measured[0].reason // null) == ($proof[0].reason // null)
          and $measured[0].hits == $proof[0].hits
          and $measured[0].misses == $proof[0].misses
          and $measured[0].bypasses == $proof[0].bypasses
          and $measured[0].bytes_restored == $proof[0].bytes_restored
        ' >/dev/null; then
          proof_outcome_identical=true
        fi
        diagnostics_evidence="$proof_diagnostics"
        diagnostics_source="unmeasured_explain_replay_from_identical_pre_run_state"
        diagnostics_replay_exit_code="$proof_exit_code"
        jq \
          --arg source "$diagnostics_source" \
          --argjson exact_argv "$proof_argv_json" \
          --argjson exit_code "$proof_exit_code" \
          --argjson diagnostics_valid "$proof_diagnostics_valid" \
          --argjson outputs_identical "$proof_outputs_identical" \
          --argjson action_census_identical "$proof_census_identical" \
          --argjson outcome_identical "$proof_outcome_identical" \
          --slurpfile measured "$measured_outcome" '
            . + {
              measured_outcome: $measured[0],
              proof_replay: {
                source: $source,
                exact_argv: $exact_argv,
                exit_code: $exit_code,
                diagnostics_valid: $diagnostics_valid,
                outputs_identical: $outputs_identical,
                action_census_identical: $action_census_identical,
                outcome_identical: $outcome_identical
              }
            }
          ' "$cache_report" >"${cache_report}.updated"
        mv "${cache_report}.updated" "$cache_report"
      else
        native_cache_report "$stdout" "$stderr" "$cache_report"
      fi
      ;;
    cargo-rail-sccache-preserved)
      native_cache_report "$stdout" "$stderr" "$cache_report"
      sccache_report "$fixture_root/$workload-sccache-live-cache" "${cache_report}.specialist"
      jq --slurpfile specialist "${cache_report}.specialist" '. + {specialist: $specialist[0]}' \
        "$cache_report" >"${cache_report}.updated"
      mv "${cache_report}.updated" "$cache_report"
      if [[ -n "$sccache_server_supervisor_pid" ]]; then
        finish_accounted_sccache_server \
          "$fixture_root/$workload-sccache-live-cache" "$timing" "$specialist_resources"
      else
        stop_sccache "$fixture_root/$workload-sccache-live-cache"
      fi
      ;;
    cargo-rail-*)
      native_cache_report "$stdout" "$stderr" "$cache_report"
      ;;
    sccache | sccache-client-side)
      local specialist_cache
      specialist_cache="$(sccache_cache_for_lane "$workload" "$lane")"
      sccache_report "$specialist_cache" "$cache_report"
      if [[ -n "$sccache_server_supervisor_pid" ]]; then
        finish_accounted_sccache_server "$specialist_cache" "$timing" "$specialist_resources"
      else
        stop_sccache "$specialist_cache"
      fi
      ;;
    *)
      printf '%s\n' '{"available":true,"kind":"native-cargo","status":"not-applicable","hits":null,"misses":null,"bypasses":null,"event_identity_digest":null,"eligible_unit_digest":null,"outcome_digest":"not-applicable","events":null,"eligible_units":null}' \
        >"$cache_report"
      ;;
  esac

  if [[ "$lane" == cargo-rail-* ]]; then
    local rail_cache="$fixture_root/$workload-$lane-cache"
    local cache_directory_present=false
    [[ -d "$rail_cache/cargo-rail/local-cas-v2" ]] && cache_directory_present=true
    jq --argjson cache_directory_present "$cache_directory_present" \
      '. + {cache_directory_present: $cache_directory_present}' "$cache_report" >"${cache_report}.updated"
    mv "${cache_report}.updated" "$cache_report"
  fi

  local diagnostics_valid=false
  local diagnostics_input="$lane_dir/diagnostics-unavailable.json"
  printf '%s\n' 'null' >"$diagnostics_input"
  if [[ "$lane" == cargo-rail-cold || "$lane" == cargo-rail-warm ]]; then
    if [[ -s "$diagnostics_evidence" ]] \
      && jq -e '.schema_version == 10 and (.phases | length == 7)' "$diagnostics_evidence" >/dev/null; then
      diagnostics_valid=true
      diagnostics_input="$diagnostics_evidence"
    fi
  else
    diagnostics_valid=true
  fi
  local exit_code
  exit_code="$(jq '.results[0].exit_codes[0] // null' "$timing")"
  jq -n \
    --arg sample_id "$workload-$round-$lane" \
    --argjson round "$round" \
    --arg workload "$workload" \
    --arg lane "$lane" \
    --argjson position "$position" \
    --arg environment_sha256 "sha256:$environment_sha256" \
    --arg binary_sha256 "sha256:$binary_sha256" \
    --arg fixture_source_sha256 "$fixture_source_sha256" \
    --arg fixture_git_commit "$fixture_git_commit" \
    --arg fixture_lock_sha256 "sha256:$fixture_lock_sha256" \
    --arg materialized_fixture_commit "$materialized_fixture_commit" \
    --arg materialized_fixture_tree "$materialized_fixture_tree" \
    --arg materialized_fixture_lock_sha256 "sha256:$materialized_fixture_lock_sha256" \
    --arg cache_state "$sample_cache_state" \
    --arg command "$sample_command" \
    --argjson argv "$sample_argv_json" \
    --argjson available "$available" \
    --argjson diagnostics_valid "$diagnostics_valid" \
    --arg diagnostics_source "$diagnostics_source" \
    --argjson diagnostics_replay_exit_code "$diagnostics_replay_exit_code" \
    --argjson exit_code "$exit_code" \
    --slurpfile diagnostics "$diagnostics_input" \
    --slurpfile timing "$timing" \
    --slurpfile manifest "$manifest" \
    --slurpfile census "$census" \
    --slurpfile cache "$cache_report" \
    --slurpfile runtime "$runtime" \
    --slurpfile specialist_resources "$specialist_resources" \
    '{
      schema_version: 12,
      sample_id: $sample_id,
      round: $round,
      workload: $workload,
      lane: $lane,
      interleave_position: $position,
      available: $available,
      identity: {
        environment: $environment_sha256,
        cargo_rail_binary: $binary_sha256,
        fixture_source: $fixture_source_sha256,
        fixture_git_dependency_commit: $fixture_git_commit,
        fixture_lockfile: $fixture_lock_sha256,
        materialized_fixture_commit: $materialized_fixture_commit,
        materialized_fixture_tree: $materialized_fixture_tree,
        materialized_fixture_lockfile: $materialized_fixture_lock_sha256,
        exact_argv: $argv,
        shell_command: $command,
        cache_state: $cache_state
      },
      resources: {
        wall_seconds: (
          if $timing[0].results[0].times[0] == null then null
          elif $specialist_resources[0].available
            then $timing[0].results[0].times[0] + $specialist_resources[0].startup_seconds
          else $timing[0].results[0].times[0]
          end
        ),
        user_seconds: (
          if $timing[0].results[0].user == null then null
          elif $specialist_resources[0].available
            then $timing[0].results[0].user + $specialist_resources[0].user_seconds
          else $timing[0].results[0].user
          end
        ),
        system_seconds: (
          if $timing[0].results[0].system == null then null
          elif $specialist_resources[0].available
            then $timing[0].results[0].system + $specialist_resources[0].system_seconds
          else $timing[0].results[0].system
          end
        ),
        cpu_seconds: (
          if $timing[0].results[0].user == null or $timing[0].results[0].system == null then null
          elif $specialist_resources[0].available then
            $timing[0].results[0].user
            + $timing[0].results[0].system
            + $specialist_resources[0].cpu_seconds
          else $timing[0].results[0].user + $timing[0].results[0].system
          end
        ),
        peak_rss_bytes: (
          if $specialist_resources[0].available then $specialist_resources[0].peak_rss_bytes
          else $timing[0].results[0].memory_usage_byte[0] // null
          end
        ),
        command_process_tree: {
          user_seconds: ($timing[0].results[0].user // null),
          system_seconds: ($timing[0].results[0].system // null),
          peak_rss_bytes: ($timing[0].results[0].memory_usage_byte[0] // null)
        },
        specialist_server: $specialist_resources[0],
        accounting: (
          if $lane == "sccache" or $lane == "sccache-client-side" or $lane == "cargo-rail-sccache-preserved" then {
            wall_time: {
              complete: true,
              scope: (
                if $specialist_resources[0].available
                then "foreground_server_startup_plus_end_to_end_command_elapsed"
                else "end_to_end_elapsed_with_automatic_server_startup"
                end
              )
            },
            cpu_time: {
              complete: $specialist_resources[0].available,
              scope: (
                if $specialist_resources[0].available
                then "command_process_tree_plus_foreground_sccache_server_and_compiler_descendants"
                else "cargo_and_sccache_clients_excluding_detached_sccache_server"
                end
              ),
              reason: (if $specialist_resources[0].available then null else $specialist_resources[0].reason end)
            },
            peak_rss: {
              complete: $specialist_resources[0].available,
              scope: (
                if $specialist_resources[0].available
                then $specialist_resources[0].peak_rss_scope
                else "command_process_tree_excluding_detached_sccache_server"
                end
              ),
              reason: (if $specialist_resources[0].available then null else $specialist_resources[0].reason end)
            }
          } else {
            wall_time: {
              complete: true,
              scope: "end_to_end_elapsed"
            },
            cpu_time: {
              complete: true,
              scope: "command_process_tree"
            },
            peak_rss: {
              complete: true,
              scope: "command_process_tree"
            }
          }
          end
        )
      },
      execution: {
        exit_code: $exit_code,
        diagnostics_valid: $diagnostics_valid,
        diagnostics_source: (if $diagnostics_source == "not_applicable" then null else $diagnostics_source end),
        diagnostics_replay_exit_code: $diagnostics_replay_exit_code,
        diagnostics: $diagnostics[0]
      },
      action_census: $census[0],
      cache: $cache[0],
      outputs: $manifest[0],
      runtime: $runtime[0]
    }' >"$lane_dir/sample.json"
}

finalize_group() {
  local group_dir="$1"
  local group_file="$group_dir/group.json"
  local temporary_group="$group_dir/.group.json.tmp"
  local total_lanes="${#lanes[@]}"
  local required_available="$total_lanes"
  if [[ "$sccache_available" != true ]]; then
    required_available=$((required_available - 3))
  fi
  jq -s --argjson total_lanes "$total_lanes" --argjson required_available "$required_available" '
    def normalized_reusable_cache_events:
      [
        (.events // [])[]
        | select((.outcome == "hit" or .outcome == "miss") and .unit_identity != null)
        | if (
            .outcome == "miss"
            and (
              .reason == "stored_verified_result"
              or (.reason | endswith(";stored_verified_result"))
            )
          )
            or (.outcome == "hit" and .reason == "verified_local_result")
          then .outcome = "reusable" | .reason = "verified_result"
          else .
          end
      ]
      | sort_by(tojson);
    def normalized_bypass_events:
      [
        (.events // [])[]
        | select(.outcome == "bypassed" or .outcome == "disabled")
        | {
            outcome,
            reason,
            unit: (
              if .unit == null then null
              else {
                descriptor: .unit.descriptor,
                output_paths: .unit.output_paths
              }
              end
            )
          }
      ]
      | sort_by(tojson);
    def event_stream_complete:
      ((.events // []) | length) == ((.hits // 0) + (.misses // 0) + (.bypasses // 0));
    def pair_outputs_identical($by_lane; $left; $right):
      ($by_lane[$left].available == $by_lane[$right].available)
      and if $by_lane[$left].available
        then $by_lane[$left].outputs.available
          and $by_lane[$right].outputs.available
          and $by_lane[$left].outputs.digest == $by_lane[$right].outputs.digest
        else true
        end;
    def action_bypass_is_exact($sample; $reason):
      $sample.cache.status == "bypassed"
      and $sample.cache.reason == $reason
      and $sample.cache.hits == 0
      and $sample.cache.misses == 0
      and $sample.cache.bypasses == 0
      and $sample.cache.setup_bytes_hashed == 0
      and $sample.cache.unit_bytes_hashed == 0
      and $sample.cache.cache_bytes_read == 0
      and $sample.cache.cache_bytes_written == 0
      and $sample.cache.bytes_restored == 0
      and ($sample.cache.cache_directory_present | not);
    sort_by(.interleave_position) as $samples
    | ($samples | map(select(.available))) as $available
    | ($samples | map({key: .lane, value: .}) | from_entries) as $by_lane
    | (
        $available
        | map(select(.lane != "native-cargo-incremental-warm" and .lane != "cargo-rail-incremental-bypass"))
      ) as $clean_profile
    | (
        $available
        | map(select(.lane == "native-cargo-incremental-warm" or .lane == "cargo-rail-incremental-bypass"))
      ) as $incremental_profile
    | {
        schema_version: 10,
        round: $samples[0].round,
        workload: $samples[0].workload,
        complete: (($samples | length) == $total_lanes and ($available | length) == $required_available),
        output_manifests_identical: (
          ($available | length) == $required_available
          and ($available | all(.outputs.available))
          and pair_outputs_identical($by_lane; "cargo-rail-cold"; "cargo-rail-warm")
        ),
        cross_root_artifacts: {
          supported: false,
          reason: "opaque compiler artifacts are source-root-bound by cache identity",
          observed_pairs_identical: (
            pair_outputs_identical($by_lane; "native-cargo-warm"; "cargo-rail-delegated")
            and pair_outputs_identical($by_lane; "native-cargo-incremental-warm"; "cargo-rail-incremental-bypass")
            and pair_outputs_identical($by_lane; "sccache"; "cargo-rail-sccache-preserved")
          )
        },
        runtime_outputs_identical: (
          $samples[0].workload != "build"
          or (
            ($available | all(.runtime.available and .runtime.exit_code == 0))
            and ($available | map(.runtime.identity) | unique | length) == 1
          )
        ),
        action_census_identical: (
          ($available | length) == $required_available
          and ($available | all(.action_census.available))
          and ($clean_profile | map(.action_census.digest) | unique | length) == 1
          and ($incremental_profile | length) == 2
          and ($incremental_profile | map(.action_census.digest) | unique | length) == 1
        ),
        bypass_cache_io_zero: (
          action_bypass_is_exact($by_lane["cargo-rail-delegated"]; "active_cargo_profile_preferred")
          and action_bypass_is_exact(
            $by_lane["cargo-rail-incremental-bypass"];
            "explicit_incremental_compilation_preserved"
          )
          and action_bypass_is_exact($by_lane["cargo-rail-disabled"]; "native_cache_disabled_by_request")
          and (
            ($by_lane["cargo-rail-sccache-preserved"].available | not)
            or action_bypass_is_exact($by_lane["cargo-rail-sccache-preserved"]; "sccache_wrapper_preserved")
          )
        ),
        cache_event_streams_complete: (
          ($by_lane["cargo-rail-cold"].cache | event_stream_complete)
          and ($by_lane["cargo-rail-warm"].cache | event_stream_complete)
        ),
        cache_proof_replays_valid: (
          [$by_lane["cargo-rail-cold"], $by_lane["cargo-rail-warm"]]
          | all(
              .cache.proof_replay.exit_code == 0
              and .cache.proof_replay.diagnostics_valid
              and .cache.proof_replay.outputs_identical
              and .cache.proof_replay.action_census_identical
              and .cache.proof_replay.outcome_identical
            )
        ),
        cache_io_consistent: (
          if ($by_lane["cargo-rail-cold"].available | not) and ($by_lane["cargo-rail-warm"].available | not)
          then true
          else $by_lane["cargo-rail-cold"].cache.cache_bytes_read == 0
            and $by_lane["cargo-rail-cold"].cache.cache_bytes_written > 0
            and $by_lane["cargo-rail-cold"].cache.bytes_restored == 0
            and $by_lane["cargo-rail-warm"].cache.cache_bytes_read
              >= $by_lane["cargo-rail-warm"].cache.bytes_restored
            and $by_lane["cargo-rail-warm"].cache.cache_bytes_written == 0
            and $by_lane["cargo-rail-warm"].cache.bytes_restored > 0
            and $by_lane["cargo-rail-cold"].cache.cache_directory_present
            and $by_lane["cargo-rail-warm"].cache.cache_directory_present
          end
        ),
        cache_equivalent: (
          ($by_lane["cargo-rail-cold"].cache) as $cold
          | ($by_lane["cargo-rail-warm"].cache) as $warm
          | if ($by_lane["cargo-rail-cold"].available | not) and ($by_lane["cargo-rail-warm"].available | not)
            then true
            else $cold.available
              and $warm.available
              and $cold.status == "reported"
              and $warm.status == "reported"
              and $cold.hits == 0
              and $warm.misses == 0
              and $warm.hits == $cold.misses
              and $warm.bypasses == $cold.bypasses
              and $warm.eligible_unit_digest == $cold.eligible_unit_digest
              and ($warm | normalized_reusable_cache_events) == ($cold | normalized_reusable_cache_events)
              and ($warm | normalized_bypass_events) == ($cold | normalized_bypass_events)
            end
        ),
        samples: $samples
      }
    | .accepted = (
        .complete
        and .output_manifests_identical
        and .runtime_outputs_identical
        and .action_census_identical
        and .bypass_cache_io_zero
        and .cache_event_streams_complete
        and .cache_proof_replays_valid
        and .cache_io_consistent
        and .cache_equivalent
        and (.samples | map(select(.available)) | all(.execution.exit_code == 0 and .execution.diagnostics_valid))
      )
    | .rejection_reasons = [
        if .complete then empty else "incomplete_interleaved_lanes" end,
        if .output_manifests_identical then empty else "output_manifests_not_identical" end,
        if .runtime_outputs_identical then empty else "runtime_outputs_not_identical" end,
        if .action_census_identical then empty else "action_census_not_identical" end,
        if .bypass_cache_io_zero then empty else "action_bypass_cache_io_or_reason_incorrect" end,
        if .cache_event_streams_complete then empty else "cache_event_stream_incomplete" end,
        if .cache_proof_replays_valid then empty else "cache_proof_replay_invalid" end,
        if .cache_io_consistent then empty else "cache_io_accounting_inconsistent" end,
        if .cache_equivalent then empty else "cache_identities_or_claimed_outputs_not_equivalent" end,
        if (.samples | map(select(.available)) | all(.execution.exit_code == 0)) then empty else "command_failed" end,
        if (.samples | map(select(.available)) | all(.execution.diagnostics_valid))
          then empty else "diagnostics_invalid"
        end
      ]
  ' "$group_dir"/*/sample.json >"$temporary_group"
  mv "$temporary_group" "$group_file"
}

warmup_lane() {
  local workload="$1"
  local lane="$2"
  prepare_lane "$workload" "$lane"
  if [[ ("$lane" == sccache || "$lane" == sccache-client-side || "$lane" == cargo-rail-sccache-preserved) \
    && "$sccache_available" != true ]]; then
    return
  fi
  local warmup_root="$fixture_root/warmup"
  mkdir -p "$warmup_root"
  local diagnostics="$warmup_root/$workload-$lane-diagnostics.json"
  local stdout="$warmup_root/$workload-$lane-stdout.txt"
  local stderr="$warmup_root/$workload-$lane-stderr.txt"
  rm -f -- "$diagnostics" "$stdout" "$stderr"
  command_for "$workload" "$lane" "$diagnostics" "$stdout" "$stderr"
  bash -c "$measured_command"
  if [[ "$lane" == sccache || "$lane" == sccache-client-side || "$lane" == cargo-rail-sccache-preserved ]]; then
    stop_sccache "$(sccache_cache_for_lane "$workload" "$lane")"
  fi
}

if [[ "$evidence_kind" == retained && "$runs" -gt 1 ]]; then
  for workload in "${workloads[@]}"; do
    for lane in "${lanes[@]}"; do
      warmup_lane "$workload" "$lane"
    done
  done
fi

for round in $(seq 1 "$attempts"); do
  for workload in "${workloads[@]}"; do
    group_dir="$results/raw/$workload/round-$round"
    if [[ -f "$group_dir/group.json" ]] && jq -e \
      --argjson round "$round" \
      --arg workload "$workload" \
      --argjson lanes "$lanes_json" \
      --arg environment "sha256:$environment_sha256" '
        .complete
        and .round == $round
        and .workload == $workload
        and (.samples | length) == ($lanes | length)
        and ([.samples[].lane] | sort) == ($lanes | sort)
        and all(.samples[]; .identity.environment == $environment)
      ' "$group_dir/group.json" >/dev/null; then
      echo "native-cache resume: keeping complete $workload round $round" >&2
      continue
    fi
    case "$group_dir" in
      "$results/raw/$workload/round-"*) rm -rf -- "$group_dir" ;;
      *)
        echo "refusing to replace benchmark group outside the result root: $group_dir" >&2
        exit 2
        ;;
    esac
    mkdir -p "$group_dir"
    rotation=$(( (round - 1) % ${#lanes[@]} ))
    for ((offset = 0; offset < ${#lanes[@]}; offset++)); do
      lane_index=$(( (rotation + offset) % ${#lanes[@]} ))
      measure_lane "$round" "$workload" "${lanes[$lane_index]}" "$offset" "$group_dir"
    done
    finalize_group "$group_dir"
    "$reporter" summarize "$results" >/dev/null
  done
done

if [[ "$l2_enabled" == true ]]; then
  measure_l2_cycle
fi

timing_wrapper="$repo_root/scripts/bench/rustc-timing-wrapper.sh"
timing_evidence_kind="$evidence_kind"
if [[ "$runs" -eq 1 ]]; then
  timing_evidence_kind=single
fi
case "$timing_evidence_kind:$(uname -s)" in
  smoke:* | single:*)
    for workload in "${workloads[@]}"; do
      printf '%s\n' \
        '{"available":false,"reason":"unit timing is omitted from single-sample runs","total_units":0,"candidate_eligible_units":0,"candidate_eligible_real_seconds":0,"bypass_real_seconds":0,"top_units":[]}' \
        >"$results/unit-$workload.json"
    done
    ;;
  *:MINGW* | *:MSYS* | *:CYGWIN*)
    for workload in "${workloads[@]}"; do
      printf '%s\n' \
        '{"available":false,"reason":"rustc timing wrapper is unavailable on native Windows","total_units":0,"candidate_eligible_units":0,"candidate_eligible_real_seconds":0,"bypass_real_seconds":0,"top_units":[]}' \
        >"$results/unit-$workload.json"
    done
    ;;
  *)
    for workload in "${workloads[@]}"; do
      timing_dir="$results/unit-$workload"
      timing_root="$fixture_root/$workload-native-cargo"
      safe_remove "$timing_root/target"
      mkdir -p "$timing_dir"
      subcommand="$(workload_cargo_subcommand "$workload")"
      timing_args=(--workspace --all-targets --all-features --locked --offline --quiet)
      if [[ "$workload" == build ]]; then
        timing_args=(--workspace --release --all-features --locked --offline --quiet)
      fi
      (
        cd "$timing_root"
        env CARGO_RAIL_UNIT_TIMINGS="$timing_dir" CARGO_RAIL_BENCH_WORKSPACE="$timing_root" \
          RUSTC_WRAPPER="$timing_wrapper" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
          cargo "$subcommand" "${timing_args[@]}"
      )
      jq -Rn '
        [inputs
          | split("\t") as $f
          | select($f | length >= 11)
          | {
              crate: $f[0], crate_type: $f[1], emit: $f[2], source: $f[3],
              test: ($f[4] == "true"), native_link: ($f[5] == "true"),
              incremental: ($f[6] == "true"), real_seconds: ($f[7] | tonumber),
              user_seconds: ($f[8] | tonumber), system_seconds: ($f[9] | tonumber),
              candidate_eligible: (
                $f[1] == "lib" and $f[4] == "false" and $f[5] == "false" and $f[6] == "false"
                and ($f[2] == "dep-info,metadata" or $f[2] == "dep-info,metadata,link")
              )
            }
        ] as $units
        | {
            available: true,
            total_units: ($units | length),
            candidate_eligible_units: ($units | map(select(.candidate_eligible)) | length),
            candidate_eligible_real_seconds: ($units | map(select(.candidate_eligible) | .real_seconds) | add // 0),
            bypass_real_seconds: ($units | map(select(.candidate_eligible | not) | .real_seconds) | add // 0),
            top_units: ($units | sort_by(.real_seconds) | reverse | .[0:15])
          }
      ' "$timing_dir"/*.tsv >"$results/unit-$workload.json"
    done
    ;;
esac

"$reporter" validate "$results"


echo "native-cache benchmark results: $results" >&2
