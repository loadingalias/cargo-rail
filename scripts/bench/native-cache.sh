#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
runs="${1:-10}"
binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/native-cache/$(date -u +%Y%m%dT%H%M%SZ)}"

[[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 3 ]] || {
  echo "native-cache benchmark runs must be an integer of at least 3" >&2
  exit 2
}
for tool in cargo git hyperfine jq; do
  command -v "$tool" >/dev/null || { echo "missing required benchmark tool: $tool" >&2; exit 2; }
done
[[ -x "$binary" ]] || { echo "build the release binary first: cargo build --release --locked" >&2; exit 2; }

unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS
unset CARGO_TARGET_DIR RUSTC RUSTC_BOOTSTRAP RUSTC_FORCE_INCREMENTAL RUSTC_WORKSPACE_WRAPPER RUSTC_WRAPPER RUSTFLAGS

mkdir -p "$results"
results="$(cd "$results" && pwd -P)"
binary_sha256=""
if command -v shasum >/dev/null 2>&1; then
  binary_sha256="$(shasum -a 256 "$binary" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  binary_sha256="$(sha256sum "$binary" | awk '{print $1}')"
fi
worktree_dirty=false
[[ -z "$(git -C "$repo_root" status --porcelain=v1)" ]] || worktree_dirty=true
jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg fixture "tests/fixtures/native_cache/real_world" \
  --arg binary "$binary" \
  --arg binary_sha256 "$binary_sha256" \
  --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --argjson worktree_dirty "$worktree_dirty" \
  --arg rustc "$(rustc -vV)" \
  --arg cargo "$(cargo -Vv)" \
  --arg host "$(uname -a)" \
  '{
    generated_at: $generated_at,
    fixture: $fixture,
    release_binary: $binary,
    release_binary_sha256: $binary_sha256,
    repository_commit: $commit,
    worktree_dirty: $worktree_dirty,
    rustc: $rustc,
    cargo: $cargo,
    host: $host,
    environment: {
      cargo_incremental: "0",
      network: "offline",
      cargo_target_dir: "fixture-local",
      compiler_overrides: "removed",
      rustflags: "removed",
      rustc_wrapper: "removed",
      rustc_workspace_wrapper: "removed"
    }
  }' >"$results/environment.json"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-native-bench.XXXXXX")"
fixture_root="$(cd "$fixture_root" && pwd -P)"
cleanup() {
  [[ -n "$fixture_root" && "$fixture_root" != / ]] && rm -rf -- "$fixture_root"
}
trap cleanup EXIT

capture_peak_rss() {
  local json="$1"
  local prepare="$2"
  local command="$3"
  if jq -e '.results[0].memory_usage_byte != null' "$json" >/dev/null; then
    return
  fi
  if [[ ! -x /usr/bin/time ]] || ! /usr/bin/time --version 2>&1 | grep -qi 'GNU time'; then
    return
  fi

  sh -c "$prepare"
  local rss_file="${json%.json}.rss-kib"
  /usr/bin/time -f '%M' -o "$rss_file" sh -c "$command"
  local rss_kib
  rss_kib="$(tail -1 "$rss_file")"
  [[ "$rss_kib" =~ ^[0-9]+$ ]] || { echo "invalid GNU time RSS result: $rss_kib" >&2; exit 2; }
  local updated="${json}.rss"
  jq --argjson rss "$((rss_kib * 1024))" '.results[0].memory_usage_byte = [$rss]' "$json" >"$updated"
  mv "$updated" "$json"
}

shared_git="$fixture_root/git-source"
materialize() {
  "$repo_root/scripts/fixtures/materialize-native-cache.sh" "$fixture_root/$1" "$shared_git" >/dev/null
}
for fixture in native-check native-build rail-check-disabled rail-build-disabled rail-check-seed rail-check-warm \
  rail-build-seed rail-build-warm timing-check timing-build; do
  materialize "$fixture"
done

materialize_binary_only() {
  local destination="$fixture_root/$1"
  mkdir -p "$destination/.config" "$destination/src"
  printf '%s\n' \
    '[package]' \
    'name = "cargo-rail-native-cache-binary-only"' \
    'version = "0.1.0"' \
    'edition = "2024"' \
    'publish = false' \
    >"$destination/Cargo.toml"
  printf '%s\n' 'fn main() {}' >"$destination/src/main.rs"
  printf '%s\n' '# Intentionally empty.' >"$destination/.config/rail.toml"
  printf '%s\n' 'target/' >"$destination/.gitignore"
  cargo generate-lockfile --manifest-path "$destination/Cargo.toml" --offline --quiet
  git -C "$destination" init --quiet --initial-branch=main --object-format=sha1
  git -C "$destination" config core.autocrlf false
  git -C "$destination" config core.filemode false
  git -C "$destination" add --all
  local tree commit
  tree="$(git -C "$destination" write-tree)"
  commit="$({
    GIT_AUTHOR_NAME='cargo-rail fixture' \
    GIT_AUTHOR_EMAIL='fixture@cargo-rail.invalid' \
    GIT_AUTHOR_DATE='2000-01-03T00:00:00Z' \
    GIT_COMMITTER_NAME='cargo-rail fixture' \
    GIT_COMMITTER_EMAIL='fixture@cargo-rail.invalid' \
    GIT_COMMITTER_DATE='2000-01-03T00:00:00Z' \
      git -C "$destination" commit-tree "$tree" -m 'Create unsupported binary-only workload'
  })"
  git -C "$destination" update-ref refs/heads/main "$commit"
}
for fixture in binary-native binary-disabled binary-active; do
  materialize_binary_only "$fixture"
done

native_check="cd '$fixture_root/native-check' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 cargo check --workspace --all-features --locked --offline --quiet"
native_build="cd '$fixture_root/native-build' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 cargo build --workspace --release --all-features --locked --offline --quiet"
rail_check="cd '$fixture_root/rail-check-warm' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR='$fixture_root/check-live-cache' '$binary' rail run --all --action build -- --all-features --locked --offline --quiet"
rail_build="cd '$fixture_root/rail-build-warm' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR='$fixture_root/build-live-cache' '$binary' rail run --all --action distribution -- --all-features --offline --quiet"
rail_check_disabled="cd '$fixture_root/rail-check-disabled' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 '$binary' rail run --all --action build --no-cache -- --all-features --locked --offline --quiet"
rail_build_disabled="cd '$fixture_root/rail-build-disabled' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 '$binary' rail run --all --action distribution --no-cache -- --all-features --offline --quiet"
binary_native="cd '$fixture_root/binary-native' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 cargo check --locked --offline --quiet"
binary_disabled="cd '$fixture_root/binary-disabled' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 '$binary' rail run --all --action build --no-cache -- --locked --offline --quiet"
binary_active="cd '$fixture_root/binary-active' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 '$binary' rail run --all --action build -- --locked --offline --quiet"

hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/native-check/target'" \
  --export-json "$results/native-check-cold.json" "$native_check"
capture_peak_rss "$results/native-check-cold.json" "rm -rf -- '$fixture_root/native-check/target'" "$native_check"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/native-build/target'" \
  --export-json "$results/native-build-cold.json" "$native_build"
capture_peak_rss "$results/native-build-cold.json" "rm -rf -- '$fixture_root/native-build/target'" "$native_build"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/rail-check-disabled/target'" \
  --export-json "$results/cargo-rail-check-disabled.json" "$rail_check_disabled"
capture_peak_rss \
  "$results/cargo-rail-check-disabled.json" \
  "rm -rf -- '$fixture_root/rail-check-disabled/target'" \
  "$rail_check_disabled"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/rail-build-disabled/target'" \
  --export-json "$results/cargo-rail-build-disabled.json" "$rail_build_disabled"
capture_peak_rss \
  "$results/cargo-rail-build-disabled.json" \
  "rm -rf -- '$fixture_root/rail-build-disabled/target'" \
  "$rail_build_disabled"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/binary-native/target'" \
  --export-json "$results/binary-only-native.json" "$binary_native"
capture_peak_rss "$results/binary-only-native.json" "rm -rf -- '$fixture_root/binary-native/target'" "$binary_native"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/binary-disabled/target'" \
  --export-json "$results/binary-only-cargo-rail-disabled.json" "$binary_disabled"
capture_peak_rss \
  "$results/binary-only-cargo-rail-disabled.json" \
  "rm -rf -- '$fixture_root/binary-disabled/target'" \
  "$binary_disabled"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/binary-active/target'" \
  --export-json "$results/binary-only-cargo-rail-bypassed.json" "$binary_active"
capture_peak_rss \
  "$results/binary-only-cargo-rail-bypassed.json" \
  "rm -rf -- '$fixture_root/binary-active/target'" \
  "$binary_active"

rail_check_cold="cd '$fixture_root/rail-check-seed' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR='$fixture_root/check-cold-cache' '$binary' rail run --all --action build -- --all-features --locked --offline --quiet"
rail_build_cold="cd '$fixture_root/rail-build-seed' && env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR='$fixture_root/build-cold-cache' '$binary' rail run --all --action distribution -- --all-features --offline --quiet"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/rail-check-seed/target' '$fixture_root/check-cold-cache'" \
  --export-json "$results/cargo-rail-check-cold.json" "$rail_check_cold"
capture_peak_rss \
  "$results/cargo-rail-check-cold.json" \
  "rm -rf -- '$fixture_root/rail-check-seed/target' '$fixture_root/check-cold-cache'" \
  "$rail_check_cold"
hyperfine --runs "$runs" \
  --prepare "rm -rf -- '$fixture_root/rail-build-seed/target' '$fixture_root/build-cold-cache'" \
  --export-json "$results/cargo-rail-build-cold.json" "$rail_build_cold"
capture_peak_rss \
  "$results/cargo-rail-build-cold.json" \
  "rm -rf -- '$fixture_root/rail-build-seed/target' '$fixture_root/build-cold-cache'" \
  "$rail_build_cold"

rm -rf -- "$fixture_root/rail-check-seed/target" "$fixture_root/check-seed-cache"
(cd "$fixture_root/rail-check-seed" && \
  env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 \
    CARGO_RAIL_CACHE_DIR="$fixture_root/check-seed-cache" \
    "$binary" rail run --all --action build -- --all-features --locked --offline --quiet \
    >/dev/null 2>&1 < /dev/null)
rm -rf -- "$fixture_root/rail-build-seed/target" "$fixture_root/build-seed-cache"
(cd "$fixture_root/rail-build-seed" && \
  env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 \
    CARGO_RAIL_CACHE_DIR="$fixture_root/build-seed-cache" \
    "$binary" rail run --all --action distribution -- --all-features --offline --quiet \
    >/dev/null 2>&1 < /dev/null)

check_warm_prepare="rm -rf -- '$fixture_root/rail-check-warm/target' '$fixture_root/check-live-cache' && cp -R '$fixture_root/check-seed-cache' '$fixture_root/check-live-cache'"
build_warm_prepare="rm -rf -- '$fixture_root/rail-build-warm/target' '$fixture_root/build-live-cache' && cp -R '$fixture_root/build-seed-cache' '$fixture_root/build-live-cache'"
hyperfine --runs "$runs" --prepare "$check_warm_prepare" \
  --export-json "$results/cargo-rail-check-warm-cross-root.json" "$rail_check"
capture_peak_rss "$results/cargo-rail-check-warm-cross-root.json" "$check_warm_prepare" "$rail_check"
hyperfine --runs "$runs" --prepare "$build_warm_prepare" \
  --export-json "$results/cargo-rail-build-warm-cross-root.json" "$rail_build"
capture_peak_rss "$results/cargo-rail-build-warm-cross-root.json" "$build_warm_prepare" "$rail_build"

measure_report() {
  local action="$1"
  local fixture="$2"
  local seed="$3"
  local live="$4"
  local output="$5"
  local diagnostics="$6"
  shift 6
  rm -rf -- "$fixture/target" "$live"
  cp -R "$seed" "$live"
  (cd "$fixture" && \
    env -u RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL=0 CARGO_RAIL_CACHE_DIR="$live" \
      "$binary" rail --diagnostics-file "$diagnostics" run --all --action "$action" --explain -- "$@" \
      >"$output" 2>&1 < /dev/null)
}
measure_report build "$fixture_root/rail-check-warm" "$fixture_root/check-seed-cache" \
  "$fixture_root/check-live-cache" "$results/check-report.txt" "$results/check-diagnostics.json" \
  --all-features --locked --offline --quiet
measure_report distribution "$fixture_root/rail-build-warm" "$fixture_root/build-seed-cache" \
  "$fixture_root/build-live-cache" "$results/build-report.txt" "$results/build-diagnostics.json" \
  --all-features --offline --quiet

report_json() {
  local report="$1"
  local diagnostics="$2"
  local output="$3"
  local line
  line="$(grep 'native compiler cache:' "$report" | tail -1)"
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
    --argjson process_bytes_hashed "$(jq '.hashed_file_bytes_read' "$diagnostics")" \
    --arg reasons "$reasons" \
    '{
      hits: $hits,
      misses: $misses,
      bypasses: $bypasses,
      unit_invocations: ($hits + $misses + $bypasses),
      hit_rate: (if ($hits + $misses + $bypasses) == 0 then 0 else $hits / ($hits + $misses + $bypasses) end),
      bypass_rate: (if ($hits + $misses + $bypasses) == 0 then 0 else $bypasses / ($hits + $misses + $bypasses) end),
      eligible_lookup_hit_rate: (if ($hits + $misses) == 0 then 0 else $hits / ($hits + $misses) end),
      setup_bytes_hashed: $setup_bytes_hashed,
      unit_bytes_hashed: $unit_bytes_hashed,
      main_process_bytes_hashed: $process_bytes_hashed,
      measured_input_bytes_hashed: ($process_bytes_hashed + $unit_bytes_hashed),
      bytes_restored: $bytes_restored,
      reasons: $reasons
    }' >"$output"
}
report_json "$results/check-report.txt" "$results/check-diagnostics.json" "$results/check-cache.json"
report_json "$results/build-report.txt" "$results/build-diagnostics.json" "$results/build-cache.json"

timing_wrapper="$repo_root/scripts/bench/rustc-timing-wrapper.sh"
for profile in check build; do
  timing_dir="$results/unit-$profile"
  mkdir -p "$timing_dir"
  if [[ "$profile" == check ]]; then
    (cd "$fixture_root/timing-check" && \
      env CARGO_RAIL_UNIT_TIMINGS="$timing_dir" CARGO_RAIL_BENCH_WORKSPACE="$fixture_root/timing-check" \
      RUSTC_WRAPPER="$timing_wrapper" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
      cargo check --workspace --all-targets --all-features --locked --offline --quiet)
  else
    (cd "$fixture_root/timing-build" && \
      env CARGO_RAIL_UNIT_TIMINGS="$timing_dir" CARGO_RAIL_BENCH_WORKSPACE="$fixture_root/timing-build" \
      RUSTC_WRAPPER="$timing_wrapper" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
      cargo build --workspace --release --all-features --locked --offline --quiet)
  fi
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
        total_units: ($units | length),
        candidate_eligible_units: ($units | map(select(.candidate_eligible)) | length),
        candidate_eligible_real_seconds: ($units | map(select(.candidate_eligible) | .real_seconds) | add // 0),
        bypass_real_seconds: ($units | map(select(.candidate_eligible | not) | .real_seconds) | add // 0),
        top_units: ($units | sort_by(.real_seconds) | reverse | .[0:15])
      }
  ' "$timing_dir"/*.tsv >"$results/unit-$profile.json"
done

sccache_available=false
sccache_bin="${SCCACHE_015_BIN:-}"
if [[ -n "$sccache_bin" && -x "$sccache_bin" ]]; then
  sccache_available=true
  materialize sccache-seed
  materialize sccache-warm
  sccache_seed="$fixture_root/sccache-seed-cache"
  sccache_live="$fixture_root/sccache-live-cache"
  sccache_command="cd '$fixture_root/sccache-warm' && env -u RUSTC_WORKSPACE_WRAPPER SCCACHE_DIR='$sccache_live' RUSTC_WRAPPER='$sccache_bin' CARGO_INCREMENTAL=0 cargo check --workspace --all-features --locked --offline --quiet"
  sccache_cold="cd '$fixture_root/sccache-seed' && env -u RUSTC_WORKSPACE_WRAPPER SCCACHE_DIR='$fixture_root/sccache-cold-cache' RUSTC_WRAPPER='$sccache_bin' CARGO_INCREMENTAL=0 cargo check --workspace --all-features --locked --offline --quiet"
  hyperfine --runs "$runs" \
    --prepare "env SCCACHE_DIR='$fixture_root/sccache-cold-cache' '$sccache_bin' --stop-server >/dev/null 2>&1 || true; rm -rf -- '$fixture_root/sccache-seed/target' '$fixture_root/sccache-cold-cache'" \
    --export-json "$results/sccache-015-check-cold.json" "$sccache_cold"
  capture_peak_rss \
    "$results/sccache-015-check-cold.json" \
    "env SCCACHE_DIR='$fixture_root/sccache-cold-cache' '$sccache_bin' --stop-server >/dev/null 2>&1 || true; rm -rf -- '$fixture_root/sccache-seed/target' '$fixture_root/sccache-cold-cache'" \
    "$sccache_cold"
  env SCCACHE_DIR="$fixture_root/sccache-cold-cache" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
  rm -rf -- "$fixture_root/sccache-seed/target" "$sccache_seed"
  (cd "$fixture_root/sccache-seed" && \
    env -u RUSTC_WORKSPACE_WRAPPER SCCACHE_DIR="$sccache_seed" RUSTC_WRAPPER="$sccache_bin" CARGO_INCREMENTAL=0 \
      cargo check --workspace --all-features --locked --offline --quiet)
  env SCCACHE_DIR="$sccache_seed" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
  sccache_prepare="env SCCACHE_DIR='$sccache_live' '$sccache_bin' --stop-server >/dev/null 2>&1 || true; rm -rf -- '$fixture_root/sccache-warm/target' '$sccache_live'; cp -R '$sccache_seed' '$sccache_live'"
  hyperfine --runs "$runs" --prepare "$sccache_prepare" \
    --export-json "$results/sccache-015-check-warm-cross-root.json" "$sccache_command"
  capture_peak_rss "$results/sccache-015-check-warm-cross-root.json" "$sccache_prepare" "$sccache_command"
  stale_dep_info="$({
    grep -R -l -F "$fixture_root/sccache-seed" "$fixture_root/sccache-warm/target" --include='*.d' 2>/dev/null || true
  } | wc -l | tr -d ' ')"
else
  printf '{"results":[{"times":[],"memory_usage_byte":[],"user":0,"system":0}]}\n' \
    >"$results/sccache-015-check-cold.json"
  cp "$results/sccache-015-check-cold.json" "$results/sccache-015-check-warm-cross-root.json"
  stale_dep_info=0
fi

jq -n \
  --argjson runs "$runs" \
  --argjson sccache_available "$sccache_available" \
  --argjson sccache_stale_dep_info "$stale_dep_info" \
  --slurpfile native_check "$results/native-check-cold.json" \
  --slurpfile native_build "$results/native-build-cold.json" \
  --slurpfile rail_check_disabled "$results/cargo-rail-check-disabled.json" \
  --slurpfile rail_build_disabled "$results/cargo-rail-build-disabled.json" \
  --slurpfile rail_check_cold "$results/cargo-rail-check-cold.json" \
  --slurpfile rail_check_warm "$results/cargo-rail-check-warm-cross-root.json" \
  --slurpfile rail_build_cold "$results/cargo-rail-build-cold.json" \
  --slurpfile rail_build_warm "$results/cargo-rail-build-warm-cross-root.json" \
  --slurpfile check_cache "$results/check-cache.json" \
  --slurpfile build_cache "$results/build-cache.json" \
  --slurpfile check_units "$results/unit-check.json" \
  --slurpfile build_units "$results/unit-build.json" \
  --slurpfile sccache_cold "$results/sccache-015-check-cold.json" \
  --slurpfile sccache_warm "$results/sccache-015-check-warm-cross-root.json" \
  --slurpfile binary_native "$results/binary-only-native.json" \
  --slurpfile binary_disabled "$results/binary-only-cargo-rail-disabled.json" \
  --slurpfile binary_bypassed "$results/binary-only-cargo-rail-bypassed.json" '
  def stats($input):
    ($input[0].results[0]) as $result
    | ($result.times | sort) as $times
    | if ($times | length) == 0 then null else {
        p50_seconds: $times[(((($times | length) * 0.50) | ceil) - 1)],
        p95_seconds: $times[(((($times | length) * 0.95) | ceil) - 1)],
        user_mean_seconds: $result.user,
        system_mean_seconds: $result.system,
        peak_rss_bytes: (($result.memory_usage_byte // []) | if length == 0 then null else max end)
      } end;
  {
    schema_version: 1,
    runs: $runs,
    native_check_cold: stats($native_check),
    cargo_rail_check_disabled: stats($rail_check_disabled),
    cargo_rail_check_cold: stats($rail_check_cold),
    cargo_rail_check_warm_cross_root: stats($rail_check_warm),
    native_build_cold: stats($native_build),
    cargo_rail_build_disabled: stats($rail_build_disabled),
    cargo_rail_build_cold: stats($rail_build_cold),
    cargo_rail_build_warm_cross_root: stats($rail_build_warm),
    unsupported_binary_only: {
      native: stats($binary_native),
      cargo_rail_disabled: stats($binary_disabled),
      cargo_rail_bypassed: stats($binary_bypassed)
    },
    check_cache: $check_cache[0],
    build_cache: $build_cache[0],
    check_units: $check_units[0],
    build_units: $build_units[0],
    sccache_0_15: {
      available: $sccache_available,
      check_cold: stats($sccache_cold),
      check_warm_cross_root: stats($sccache_warm),
      stale_dep_info_files: $sccache_stale_dep_info
    }
  }' | tee "$results/summary.json"

echo "native-cache benchmark results: $results" >&2
