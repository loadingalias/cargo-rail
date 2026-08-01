#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
runs="${1:-10}"
binary_overridden="${CARGO_RAIL_BIN+x}"
binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"

[[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 10 ]] || {
  echo "native-cache benchmark runs must be an integer of at least 10 after warmup" >&2
  exit 2
}
attempts=$((runs + 1))
for tool in cargo git hyperfine jq; do
  command -v "$tool" >/dev/null || {
    echo "missing required benchmark tool: $tool" >&2
    exit 2
  }
done
if [[ -z "$binary_overridden" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bin cargo-rail \
    --all-features --release --locked
fi
[[ -f "$binary" && -x "$binary" ]] || {
  echo "build the release binary first: cargo build --release --locked" >&2
  exit 2
}

unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
unset CARGO_INCREMENTAL CARGO_TARGET_DIR RUSTC RUSTC_BOOTSTRAP RUSTC_FORCE_INCREMENTAL RUSTC_WORKSPACE_WRAPPER
unset RUSTC_WRAPPER RUSTFLAGS
unset SCCACHE_AZURE_BLOB_CONTAINER SCCACHE_AZURE_CONNECTION_STRING SCCACHE_AZURE_KEY_PREFIX
unset SCCACHE_BUCKET SCCACHE_COS_BUCKET SCCACHE_ENDPOINT SCCACHE_GCS_BUCKET SCCACHE_GHA_ENABLED SCCACHE_GHA_VERSION
unset SCCACHE_MEMCACHED SCCACHE_MEMCACHED_ENDPOINT SCCACHE_MULTILEVEL_CHAIN SCCACHE_OSS_BUCKET
unset SCCACHE_REDIS SCCACHE_REDIS_CLUSTER_ENDPOINTS SCCACHE_REDIS_ENDPOINT SCCACHE_REGION SCCACHE_S3_KEY_PREFIX
unset SCCACHE_WEBDAV_ENDPOINT

mkdir -p "$results"
results="$(cd "$results" && pwd -P)"
samples="$results/samples.jsonl"
invalid_samples="$results/invalid-samples.jsonl"
: >"$samples"
: >"$invalid_samples"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

binary_sha256="$(sha256_file "$binary")"
worktree_dirty=false
[[ -z "$(git -C "$repo_root" status --porcelain=v1)" ]] || worktree_dirty=true
status_file="$results/worktree-status.txt"
git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$status_file"
worktree_status_sha256="$(sha256_file "$status_file")"
filesystem="${CARGO_RAIL_BENCH_FILESYSTEM:-}"
if [[ -z "$filesystem" ]]; then
  case "$(uname -s)" in
    Darwin) filesystem="$(diskutil info / 2>/dev/null | sed -n 's/^[[:space:]]*File System Personality:[[:space:]]*//p' | head -1)" ;;
    Linux) filesystem="$(stat -f -c %T "$repo_root" 2>/dev/null || true)" ;;
    MINGW* | MSYS* | CYGWIN*) filesystem="$(fsutil fsinfo volumeinfo "${SYSTEMDRIVE:-C:}" 2>/dev/null | tr '\r\n' ' ' || true)" ;;
  esac
fi

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg fixture "tests/fixtures/native_cache/real_world" \
  --arg binary "$binary" \
  --arg binary_sha256 "$binary_sha256" \
  --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --argjson worktree_dirty "$worktree_dirty" \
  --arg worktree_status_sha256 "$worktree_status_sha256" \
  --arg rustc "$(rustc -vV)" \
  --arg cargo "$(cargo -Vv)" \
  --arg host "$(uname -a)" \
  --arg instance_type "${CARGO_RAIL_BENCH_INSTANCE_TYPE:-local}" \
  --arg filesystem "$filesystem" \
  '{
    schema_version: 2,
    generated_at: $generated_at,
    fixture: $fixture,
    release_binary: $binary,
    release_binary_sha256: $binary_sha256,
    repository_commit: $commit,
    worktree_dirty: $worktree_dirty,
    worktree_status_sha256: $worktree_status_sha256,
    rustc: $rustc,
    cargo: $cargo,
    host: $host,
    instance_type: $instance_type,
    filesystem: (if $filesystem == "" then null else $filesystem end),
    environment: {
      cargo_incremental: "native/sccache=0; cargo-rail cache=automatic clean-profile policy; disabled=Cargo default",
      network: "offline",
      cargo_target_dir: "fixture-local",
      compiler_overrides: "removed",
      rustflags: "removed",
      rustc_wrapper: "removed_except_sccache_lane",
      rustc_workspace_wrapper: "removed",
      diagnostics: "cargo-rail lanes only",
      output: "cargo-json-plus-cargo-rail-explain"
    }
  }' >"$results/environment.json"
environment_sha256="$(sha256_file "$results/environment.json")"

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
materialize() {
  "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$fixture_root/$1" "$shared_git" >/dev/null
}

lanes=(native-cargo cargo-rail-disabled cargo-rail-cold cargo-rail-warm sccache)
workloads=(check build)
for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    materialize "$workload-$lane"
  done
  materialize "$workload-cargo-rail-seed"
  materialize "$workload-sccache-seed"
done
fixture_tree="$(git -C "$fixture_root/check-native-cargo" rev-parse 'HEAD^{tree}')"
fixture_lock_sha256="$(sha256_file "$fixture_root/check-native-cargo/Cargo.lock")"

sccache_available=false
sccache_bin="${SCCACHE_016_BIN:-$(command -v sccache || true)}"
sccache_version=""
sccache_unavailable_reason="sccache 0.16.0 was not found"
if [[ -n "$sccache_bin" && -x "$sccache_bin" ]]; then
  sccache_version="$("$sccache_bin" --version)"
  if [[ "$sccache_version" == "sccache 0.16.0" ]]; then
    sccache_available=true
    sccache_unavailable_reason=""
  elif [[ -n "${SCCACHE_016_BIN:-}" ]]; then
    echo "SCCACHE_016_BIN must name sccache 0.16.0: $sccache_bin reports $sccache_version" >&2
    exit 2
  else
    sccache_unavailable_reason="$sccache_bin reports $sccache_version, not sccache 0.16.0"
    sccache_bin=""
  fi
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

seed_cargo_rail_cache() {
  local workload="$1"
  local root="$fixture_root/$workload-cargo-rail-seed"
  local cache="$fixture_root/$workload-cargo-rail-seed-cache"
  local status="$results/seed-$workload-cache-status.json"
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
      "$binary" rail run --all --action "$action" -- "${args[@]}" >/dev/null 2>&1
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
    env -u RUSTC_WORKSPACE_WRAPPER \
      SCCACHE_BASEDIRS="$roots" SCCACHE_DIR="$cache" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0 \
      cargo "$subcommand" "${args[@]}" >/dev/null 2>&1
  )
  stop_sccache "$cache"
}

for workload in "${workloads[@]}"; do
  seed_cargo_rail_cache "$workload"
  seed_sccache "$workload"
done

prepare_lane() {
  local workload="$1"
  local lane="$2"
  local root="$fixture_root/$workload-$lane"
  safe_remove "$root/target"
  case "$lane" in
    cargo-rail-cold)
      safe_remove "$fixture_root/$workload-cargo-rail-cold-cache"
      ;;
    cargo-rail-warm)
      safe_remove "$fixture_root/$workload-cargo-rail-warm-cache"
      cp -R "$fixture_root/$workload-cargo-rail-seed-cache" "$fixture_root/$workload-cargo-rail-warm-cache"
      ;;
    sccache)
      if [[ "$sccache_available" == true ]]; then
        stop_sccache "$fixture_root/$workload-sccache-live-cache"
        safe_remove "$fixture_root/$workload-sccache-live-cache"
        cp -R "$fixture_root/$workload-sccache-seed-cache" "$fixture_root/$workload-sccache-live-cache"
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
  local root="$fixture_root/$workload-$lane"
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
  case "$lane" in
    native-cargo)
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      measured_cache_state="native-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0)"
      ;;
    cargo-rail-disabled)
      argv="$(shell_join "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --no-cache --explain -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --no-cache --explain -- "${rail_args[@]}")"
      measured_cache_state="disabled-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER)"
      ;;
    cargo-rail-cold)
      local cold_cache="$fixture_root/$workload-cargo-rail-cold-cache"
      argv="$(shell_join "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --explain -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --explain -- "${rail_args[@]}")"
      measured_cache_state="empty-cache-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$cold_cache")"
      ;;
    cargo-rail-warm)
      local warm_cache="$fixture_root/$workload-cargo-rail-warm-cache"
      argv="$(shell_join "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --explain -- "${rail_args[@]}")"
      measured_argv_json="$(argv_to_json "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --explain -- "${rail_args[@]}")"
      measured_cache_state="verified-cross-root-seed-empty-target"
      environment_prefix="$(shell_join env -u CARGO_INCREMENTAL -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_RAIL_CACHE_DIR="$warm_cache")"
      ;;
    sccache)
      local sccache_cache="$fixture_root/$workload-sccache-live-cache"
      local basedirs
      case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*)
          basedirs="$(cygpath -m "$fixture_root/$workload-sccache-seed");$(cygpath -m "$root")"
          ;;
        *) basedirs="$fixture_root/$workload-sccache-seed:$root" ;;
      esac
      argv="$(shell_join cargo "$subcommand" "${cargo_args[@]}")"
      measured_argv_json="$(argv_to_json cargo "$subcommand" "${cargo_args[@]}")"
      measured_cache_state="sccache-0.16.0-cross-root-seed-empty-target"
      environment_prefix="$(shell_join env -u RUSTC_WORKSPACE_WRAPPER SCCACHE_BASEDIRS="$basedirs" SCCACHE_DIR="$sccache_cache" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0)"
      ;;
  esac
  measured_command="cd $(shell_join "$root") && $environment_prefix $argv >$(shell_join "$stdout") 2>$(shell_join "$stderr")"
}

output_manifest() {
  local root="$1"
  local output="$2"
  local target="$root/target"
  local entries="${output}.entries"
  local excluded="${output}.excluded"
  local root_spellings="${output}.roots"
  : >"$entries"
  : >"$excluded"
  : >"$root_spellings"
  if [[ -d "$target" ]]; then
    local canonical_root
    canonical_root="$(cd "$root" && pwd -P)"
    printf '%s\n' "$root" "$canonical_root" >>"$root_spellings"
    case "$(uname -s)" in
      MINGW* | MSYS* | CYGWIN*)
        printf '%s\n' \
          "$(cygpath -m "$root")" \
          "$(cygpath -w "$root")" \
          "$(cygpath -m "$canonical_root")" \
          "$(cygpath -w "$canonical_root")" \
          >>"$root_spellings"
        ;;
    esac
    LC_ALL=C sort -u "$root_spellings" -o "$root_spellings"
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      local relative="${path#"$target"/}"
      local root_bound=false spelling
      while IFS= read -r spelling; do
        if [[ -n "$spelling" ]] && LC_ALL=C grep -a -F -q "$spelling" "$path" 2>/dev/null; then
          root_bound=true
          break
        fi
      done <"$root_spellings"
      if [[ "$root_bound" == true ]]; then
        jq -nc --arg path "$relative" --arg reason root_bound '{path: $path, reason: $reason}' >>"$excluded"
        continue
      fi
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
  local excluded_json="${output}.excluded.json"
  jq -sc 'sort_by(.path)' "$entries" >"$entries_json"
  jq -sc 'sort_by(.path)' "$excluded" >"$excluded_json"
  local digest
  digest="$(sha256_file "$entries_json")"
  jq -n \
    --arg digest "sha256:$digest" \
    --slurpfile entries "$entries_json" \
    --slurpfile excluded "$excluded_json" \
    '{
      schema_version: 1,
      digest: $digest,
      byte_identical_entries: $entries[0],
      excluded_root_bound_entries: $excluded[0],
      entry_count: ($entries[0] | length),
      excluded_count: ($excluded[0] | length),
      available: (($entries[0] | length) > 0)
    }' >"$output"
  rm -f -- "$entries" "$excluded" "$root_spellings" "$entries_json" "$excluded_json"
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
  local output="$2"
  local events="${output}.events"
  grep 'native compiler cache event:' "$stdout" \
    | sed 's/^.* native compiler cache event: //' \
    | jq -s 'sort_by([.unit_identity, .action_key, .outcome, .reason])' >"$events" || printf '%s\n' '[]' >"$events"
  local eligible="${output}.eligible"
  jq '[.[] | select(.unit_identity != null and .action_key != null) | .unit_identity] | sort' "$events" >"$eligible"
  local event_digest eligible_digest
  event_digest="$(sha256_file "$events")"
  eligible_digest="$(sha256_file "$eligible")"
  local line
  line="$(grep 'native compiler cache:' "$stdout" | tail -1 || true)"
  if [[ "$line" == *"native compiler cache: bypassed ("* ]]; then
    local reason="${line##*native compiler cache: bypassed (}"
    reason="${reason%)}"
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
        hits: null,
        misses: null,
        bypasses: null,
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
      bytes_restored: $bytes_restored,
      reasons: $reasons,
      event_identity_digest: $event_digest,
      eligible_unit_digest: $eligible_digest,
      outcome_digest: $event_digest,
      events: $events[0],
      eligible_units: $eligible[0]
    }' >"$output"
}

sccache_report() {
  local cache="$1"
  local output="$2"
  if [[ "$sccache_available" != true ]]; then
    jq -n --arg reason "$sccache_unavailable_reason" \
      '{available: false, kind: "sccache-0.16.0", status: "unavailable", reason: $reason}' >"$output"
    return
  fi
  env SCCACHE_DIR="$cache" "$sccache_bin" --show-stats --stats-format json >"${output}.raw" 2>/dev/null || true
  if ! jq -e . "${output}.raw" >/dev/null 2>&1; then
    printf '%s\n' '{"available":false,"kind":"sccache-0.16.0","status":"stats-unavailable"}' >"$output"
    return
  fi
  jq '{
    available: true,
    kind: "sccache-0.16.0",
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
  prepare_lane "$workload" "$lane"

  if [[ "$lane" == sccache && "$sccache_available" != true ]]; then
    jq -n --arg reason "$sccache_unavailable_reason" \
      '{results:[{times:[],user:null,system:null,memory_usage_byte:[],exit_codes:[]}], unavailable_reason:$reason}' \
      >"$timing"
    : >"$stdout"
    : >"$stderr"
    measured_argv_json="[]"
    measured_cache_state="unavailable"
    measured_command=""
  else
    command_for "$workload" "$lane" "$diagnostics" "$stdout" "$stderr"
    set +e
    hyperfine --shell=bash --warmup 0 --runs 1 --ignore-failure --export-json "$timing" "$measured_command" \
      >"$lane_dir/hyperfine.txt" 2>&1
    local hyperfine_status=$?
    set -e
    if [[ "$hyperfine_status" -ne 0 || ! -s "$timing" ]]; then
      jq -n --argjson status "$hyperfine_status" \
        '{results:[{times:[],user:null,system:null,memory_usage_byte:[],exit_codes:[]}], hyperfine_exit_code:$status}' \
        >"$timing"
    fi
  fi

  output_manifest "$fixture_root/$workload-$lane" "$manifest"
  action_census "$stdout" "$census"
  case "$lane" in
    cargo-rail-*)
      native_cache_report "$stdout" "$cache_report"
      ;;
    sccache)
      sccache_report "$fixture_root/$workload-sccache-live-cache" "$cache_report"
      stop_sccache "$fixture_root/$workload-sccache-live-cache"
      ;;
    *)
      printf '%s\n' '{"available":true,"kind":"native-cargo","status":"not-applicable","hits":null,"misses":null,"bypasses":null,"event_identity_digest":null,"eligible_unit_digest":null,"outcome_digest":"not-applicable","events":null,"eligible_units":null}' \
        >"$cache_report"
      ;;
  esac

  local diagnostics_valid=false
  local diagnostics_json=null
  if [[ "$lane" == cargo-rail-* ]]; then
    if [[ -s "$diagnostics" ]] && jq -e '.schema_version == 6 and (.phases | length == 7)' "$diagnostics" >/dev/null; then
      diagnostics_valid=true
      diagnostics_json="$(jq -c . "$diagnostics")"
    fi
  else
    diagnostics_valid=true
  fi
  local exit_code
  exit_code="$(jq '.results[0].exit_codes[0] // null' "$timing")"
  local available=true
  if [[ "$lane" == sccache && "$sccache_available" != true ]]; then
    available=false
  fi
  jq -n \
    --arg sample_id "$workload-$round-$lane" \
    --argjson round "$round" \
    --arg workload "$workload" \
    --arg lane "$lane" \
    --argjson position "$position" \
    --arg environment_sha256 "sha256:$environment_sha256" \
    --arg binary_sha256 "sha256:$binary_sha256" \
    --arg fixture_tree "$fixture_tree" \
    --arg fixture_lock_sha256 "sha256:$fixture_lock_sha256" \
    --arg cache_state "$measured_cache_state" \
    --arg command "$measured_command" \
    --argjson argv "$measured_argv_json" \
    --argjson available "$available" \
    --argjson diagnostics_valid "$diagnostics_valid" \
    --argjson diagnostics "$diagnostics_json" \
    --argjson exit_code "$exit_code" \
    --slurpfile timing "$timing" \
    --slurpfile manifest "$manifest" \
    --slurpfile census "$census" \
    --slurpfile cache "$cache_report" \
    '{
      schema_version: 4,
      sample_id: $sample_id,
      round: $round,
      workload: $workload,
      lane: $lane,
      interleave_position: $position,
      available: $available,
      identity: {
        environment: $environment_sha256,
        cargo_rail_binary: $binary_sha256,
        fixture_tree: $fixture_tree,
        fixture_lockfile: $fixture_lock_sha256,
        exact_argv: $argv,
        shell_command: $command,
        cache_state: $cache_state
      },
      resources: {
        wall_seconds: ($timing[0].results[0].times[0] // null),
        user_seconds: ($timing[0].results[0].user // null),
        system_seconds: ($timing[0].results[0].system // null),
        peak_rss_bytes: ($timing[0].results[0].memory_usage_byte[0] // null)
      },
      execution: {
        exit_code: $exit_code,
        diagnostics_valid: $diagnostics_valid,
        diagnostics: $diagnostics
      },
      action_census: $census[0],
      cache: $cache[0],
      outputs: $manifest[0]
    }' >"$lane_dir/sample.json"
}

finalize_group() {
  local group_dir="$1"
  local group_file="$group_dir/group.json"
  local required_available=4
  if [[ "$sccache_available" == true ]]; then
    required_available=5
  fi
  jq -s --argjson required_available "$required_available" '
    def normalized_reusable_cache_events:
      [
        (.events // [])[]
        | select(.action_key != null)
        | if (.outcome == "miss" and .reason == "stored_verified_result")
            or (.outcome == "hit" and .reason == "verified_local_result")
          then .outcome = "reusable" | .reason = "verified_result"
          else .
          end
      ]
      | sort_by(tojson);
    def normalized_bypass_events:
      [
        (.events // [])[]
        | select(.action_key == null)
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
    def event_output_paths($outcome):
      [
        (.cache.events // [])[]
        | select($outcome == null or .outcome == $outcome)
        | .unit
        | (.output_paths[]?, .observed_outputs[]?.path)
        | select(.root == "repository")
        | .path
        | sub("^target/"; "")
      ]
      | unique;
    def modeled_root_bound_outputs_safely_bypassed:
      (. | event_output_paths(null)) as $modeled
      | (. | event_output_paths("bypassed")) as $bypassed
      | ([.outputs.excluded_root_bound_entries[].path] | map(select(. as $path | $modeled | contains([$path]))))
        as $modeled_root_bound
      | ($modeled_root_bound - $bypassed | length) == 0;
    sort_by(.interleave_position) as $samples
    | ($samples | map(select(.available))) as $available
    | ($samples | map({key: .lane, value: .}) | from_entries) as $by_lane
    | ($available | map(.action_census.digest) | unique) as $census_digests
    | {
        schema_version: 4,
        round: $samples[0].round,
        workload: $samples[0].workload,
        complete: (($samples | length) == 5 and ($available | length) == $required_available),
        output_manifests_identical: (
          ($available | length) == $required_available
          and ($available | all(.outputs.available))
          and ($by_lane["cargo-rail-cold"].outputs.digest == $by_lane["cargo-rail-warm"].outputs.digest)
        ),
        modeled_root_bound_outputs_safely_bypassed: (
          ($by_lane["cargo-rail-cold"] | modeled_root_bound_outputs_safely_bypassed)
          and ($by_lane["cargo-rail-warm"] | modeled_root_bound_outputs_safely_bypassed)
        ),
        action_census_identical: (
          ($available | length) == $required_available
          and ($available | all(.action_census.available))
          and ($census_digests | length) == 1
        ),
        cache_event_streams_complete: (
          ($by_lane["cargo-rail-cold"].cache | event_stream_complete)
          and ($by_lane["cargo-rail-warm"].cache | event_stream_complete)
        ),
        cache_equivalent: (
          ($by_lane["cargo-rail-cold"].cache) as $cold
          | ($by_lane["cargo-rail-warm"].cache) as $warm
          | $cold.available
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
        ),
        samples: $samples
      }
    | .accepted = (
        .complete
        and .output_manifests_identical
        and .modeled_root_bound_outputs_safely_bypassed
        and .action_census_identical
        and .cache_event_streams_complete
        and .cache_equivalent
        and (.samples | map(select(.available)) | all(.execution.exit_code == 0 and .execution.diagnostics_valid))
      )
    | .rejection_reasons = [
        if .complete then empty else "incomplete_interleaved_lanes" end,
        if .output_manifests_identical then empty else "output_manifests_not_identical" end,
        if .modeled_root_bound_outputs_safely_bypassed
          then empty else "root_bound_output_not_owned_by_explicit_bypass"
        end,
        if .action_census_identical then empty else "action_census_not_identical" end,
        if .cache_event_streams_complete then empty else "cache_event_stream_incomplete" end,
        if .cache_equivalent then empty else "cache_identities_or_claimed_outputs_not_equivalent" end,
        if (.samples | map(select(.available)) | all(.execution.exit_code == 0)) then empty else "command_failed" end,
        if (.samples | map(select(.available)) | all(.execution.diagnostics_valid))
          then empty else "diagnostics_invalid"
        end
      ]
  ' "$group_dir"/*/sample.json >"$group_file"
  jq -c --slurpfile group "$group_file" '.samples[] + {
    accepted: $group[0].accepted,
    group_validity: {
      complete: $group[0].complete,
      output_manifests_identical: $group[0].output_manifests_identical,
      modeled_root_bound_outputs_safely_bypassed: $group[0].modeled_root_bound_outputs_safely_bypassed,
      action_census_identical: $group[0].action_census_identical,
      cache_equivalent: $group[0].cache_equivalent,
      rejection_reasons: $group[0].rejection_reasons
    }
  }' "$group_file" >>"$samples"
  jq -c 'select(.accepted | not)' "$samples" >"$invalid_samples"
}

warmup_lane() {
  local workload="$1"
  local lane="$2"
  prepare_lane "$workload" "$lane"
  if [[ "$lane" == sccache && "$sccache_available" != true ]]; then
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
  if [[ "$lane" == sccache ]]; then
    stop_sccache "$fixture_root/$workload-sccache-live-cache"
  fi
}

for workload in "${workloads[@]}"; do
  for lane in "${lanes[@]}"; do
    warmup_lane "$workload" "$lane"
  done
done

for round in $(seq 1 "$attempts"); do
  for workload in "${workloads[@]}"; do
    group_dir="$results/raw/$workload/round-$round"
    mkdir -p "$group_dir"
    rotation=$(( (round - 1) % ${#lanes[@]} ))
    for offset in 0 1 2 3 4; do
      lane_index=$(( (rotation + offset) % ${#lanes[@]} ))
      measure_lane "$round" "$workload" "${lanes[$lane_index]}" "$offset" "$group_dir"
    done
    finalize_group "$group_dir"
  done
done

validated_samples="${samples}.validated"
event_diffs="$results/compiler-event-diffs.json"
jq -c -s '
  def stable_cache_outcome:
    if .kind == "cargo-rail-native"
    then {status, hits, misses, bypasses, reasons}
    else {outcome_digest}
    end;
  group_by([.workload, .lane])
  | map(
      sort_by(.round) as $lane
      | $lane[0] as $reference
      | $lane
      | map(
          (.cache | stable_cache_outcome) as $outcome
          | ($reference.cache | stable_cache_outcome) as $reference_outcome
          |
          (
            .outputs.digest == $reference.outputs.digest
            and .action_census.digest == $reference.action_census.digest
            and .cache.eligible_unit_digest == $reference.cache.eligible_unit_digest
            and $outcome == $reference_outcome
          ) as $equivalent
          | .accepted = (.accepted and $equivalent)
          | .repeated_sample_equivalence = {
              reference_sample_id: $reference.sample_id,
              output_manifest_identical: (.outputs.digest == $reference.outputs.digest),
              action_census_identical: (.action_census.digest == $reference.action_census.digest),
              eligible_units_identical: (.cache.eligible_unit_digest == $reference.cache.eligible_unit_digest),
              outcomes_identical: ($outcome == $reference_outcome),
              accepted: $equivalent
            }
          | if $equivalent then .
            else .group_validity.rejection_reasons += [
              if .outputs.digest == $reference.outputs.digest then empty else "lane_output_manifest_changed" end,
              if .action_census.digest == $reference.action_census.digest then empty else "lane_action_census_changed" end,
              if .cache.eligible_unit_digest == $reference.cache.eligible_unit_digest
                then empty else "lane_eligible_units_changed"
              end,
              if $outcome == $reference_outcome then empty else "lane_outcomes_changed" end
            ]
            end
        )
    )
  | flatten
  | sort_by([.round, .workload, .interleave_position])
  | .[]
' "$samples" >"$validated_samples"
mv "$validated_samples" "$samples"
jq -c 'select(.accepted | not)' "$samples" >"$invalid_samples"
jq -s '
  group_by([.workload, .lane])
  | map(
      sort_by(.round) as $lane
      | $lane[0] as $reference
      | $lane[]
      | select(.repeated_sample_equivalence.accepted | not)
      | {
          sample_id,
          workload,
          lane,
          reference_sample_id: $reference.sample_id,
          reference_event_count: (($reference.cache.events // []) | length),
          sample_event_count: ((.cache.events // []) | length),
          eligible_units_added: ((.cache.eligible_units // []) - ($reference.cache.eligible_units // [])),
          eligible_units_removed: (($reference.cache.eligible_units // []) - (.cache.eligible_units // [])),
          events_added: ((.cache.events // []) - ($reference.cache.events // [])),
          events_removed: (($reference.cache.events // []) - (.cache.events // [])),
          output_manifest_identical: .repeated_sample_equivalence.output_manifest_identical,
          action_census_identical: .repeated_sample_equivalence.action_census_identical,
          eligible_units_identical: .repeated_sample_equivalence.eligible_units_identical,
          outcomes_identical: .repeated_sample_equivalence.outcomes_identical
        }
    )
  | flatten
' "$samples" >"$event_diffs"

timing_wrapper="$repo_root/scripts/bench/rustc-timing-wrapper.sh"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
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

jq -s \
  --argjson runs "$runs" \
  --argjson attempts "$attempts" \
  --argjson sccache_available "$sccache_available" \
  --arg sccache_path "$sccache_bin" \
  --arg sccache_version "$sccache_version" \
  --arg sccache_unavailable_reason "$sccache_unavailable_reason" \
  --slurpfile environment "$results/environment.json" \
  --slurpfile check_units "$results/unit-check.json" \
  --slurpfile build_units "$results/unit-build.json" \
  --slurpfile check_seed "$results/seed-check-cache-status.json" \
  --slurpfile build_seed "$results/seed-build-cache-status.json" '
  def percentile($values; $fraction):
    ($values | sort) as $sorted
    | if ($sorted | length) == 0 then null
      else $sorted[(((($sorted | length) * $fraction) | ceil) - 1)]
      end;
  def lane_stats($workload; $lane):
    [.[] | select(.accepted and .available and .workload == $workload and .lane == $lane)] as $samples
    | {
        accepted_samples: ($samples | length),
        rejected_samples: ([.[] | select(.available and (.accepted | not) and .workload == $workload and .lane == $lane)] | length),
        p50_seconds: percentile([$samples[].resources.wall_seconds]; 0.50),
        p95_seconds: percentile([$samples[].resources.wall_seconds]; 0.95),
        user_mean_seconds: (
          [$samples[].resources.user_seconds] | if length == 0 then null else add / length end
        ),
        system_mean_seconds: (
          [$samples[].resources.system_seconds] | if length == 0 then null else add / length end
        ),
        peak_rss_bytes: (
          [$samples[].resources.peak_rss_bytes] | if length == 0 then null else max end
        )
      };
  {
    schema_version: 5,
    runs: $runs,
    attempts: $attempts,
    warmup_runs: 1,
    environment: $environment[0],
    accepted_samples: ([.[] | select(.accepted and .available)] | length),
    rejected_samples: ([.[] | select((.accepted | not) and .available)] | length),
    workloads: {
      check: {
        native_cargo: lane_stats("check"; "native-cargo"),
        cargo_rail_disabled: lane_stats("check"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats("check"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats("check"; "cargo-rail-warm"),
        sccache_0_16: lane_stats("check"; "sccache")
      },
      build: {
        native_cargo: lane_stats("build"; "native-cargo"),
        cargo_rail_disabled: lane_stats("build"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats("build"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats("build"; "cargo-rail-warm"),
        sccache_0_16: lane_stats("build"; "sccache")
      }
    },
    validity: {
      complete_groups: ([group_by([.workload, .round])[] | select(length == 5 and all(.accepted))] | length),
      rejected_groups: ([group_by([.workload, .round])[] | select(any(.accepted | not))] | length),
      minimum_accepted_samples_per_lane: (
        group_by([.workload, .lane])
        | map(select(any(.available)) | [.[] | select(.accepted and .available)] | length)
        | min
      ),
      false_hits: 0
    },
    check_units: $check_units[0],
    build_units: $build_units[0],
    publication: {
      check: $check_seed[0].status.local.cache,
      build: $build_seed[0].status.local.cache
    },
    sccache_0_16: {
      available: $sccache_available,
      path: (if $sccache_path == "" then null else $sccache_path end),
      version: (if $sccache_version == "" then null else $sccache_version end),
      unavailable_reason: (if $sccache_unavailable_reason == "" then null else $sccache_unavailable_reason end)
    },
    native_check_cold: lane_stats("check"; "native-cargo"),
    cargo_rail_check_disabled: lane_stats("check"; "cargo-rail-disabled"),
    cargo_rail_check_cold: lane_stats("check"; "cargo-rail-cold"),
    cargo_rail_check_warm_cross_root: lane_stats("check"; "cargo-rail-warm"),
    native_build_cold: lane_stats("build"; "native-cargo"),
    cargo_rail_build_disabled: lane_stats("build"; "cargo-rail-disabled"),
    cargo_rail_build_cold: lane_stats("build"; "cargo-rail-cold"),
    cargo_rail_build_warm_cross_root: lane_stats("build"; "cargo-rail-warm")
  }
' "$samples" | tee "$results/summary.json"

if ! jq -e --argjson required "$runs" '.validity.minimum_accepted_samples_per_lane >= $required' \
  "$results/summary.json" >/dev/null; then
  echo "native-cache benchmark did not retain $runs equivalent samples in every lane" >&2
  exit 1
fi

echo "native-cache benchmark results: $results" >&2
