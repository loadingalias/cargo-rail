#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
packages="${1:-25}"
runs="${2:-10}"
binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/unify/$(date -u +%Y%m%dT%H%M%SZ)}"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-unify-bench.XXXXXX")"
cache_base="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-unify-cache.XXXXXX")"
dependency_root="$fixture/dependencies/bench-dependency"
lock="$repo_root/target/benchmarks/.unify.lock"
mkdir -p "$(dirname "$lock")"
mkdir "$lock" 2>/dev/null || { echo "unify benchmark is already running: $lock" >&2; exit 2; }
trap 'rm -rf "$fixture" "$cache_base"; rmdir "$lock"' EXIT

for tool in hyperfine jq cargo git; do
  command -v "$tool" >/dev/null || { echo "missing required benchmark tool: $tool" >&2; exit 2; }
done
[[ -x "$binary" ]] || { echo "build the release binary first: cargo build --release --locked" >&2; exit 2; }
if [[ -e "$results" ]]; then
  [[ -d "$results" && -z "$(find "$results" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
    echo "benchmark result directory must not already contain evidence: $results" >&2
    exit 2
  }
else
  mkdir -p "$results"
fi
results="$(cd "$(dirname "$results")" && pwd)/$(basename "$results")"

sha256_file() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$results/worktree-status.txt"
git -C "$repo_root" diff --binary HEAD -- >"$results/worktree.diff"
rustc -vV >"$results/rustc.txt"
cargo -Vv >"$results/cargo.txt"
uname -a >"$results/uname.txt"
jq -n \
  --arg repository_commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --arg worktree_status_sha256 "$(sha256_file "$results/worktree-status.txt")" \
  --arg worktree_diff_sha256 "$(sha256_file "$results/worktree.diff")" \
  --arg binary_sha256 "$(sha256_file "$binary")" \
  --arg binary "$binary" \
  --arg platform "$(uname -s)" \
  --arg machine "$(uname -m)" \
  '{
    schema_version: 1,
    repository_commit: $repository_commit,
    worktree_status_sha256: $worktree_status_sha256,
    worktree_diff_sha256: $worktree_diff_sha256,
    binary_sha256: $binary_sha256,
    binary: $binary,
    platform: $platform,
    machine: $machine
  }' >"$results/environment.json"

"$repo_root/tests/support/generate-workspace.sh" "$packages" "$fixture"
cp "$repo_root/rust-toolchain.toml" "$fixture/rust-toolchain.toml"

mkdir -p "$dependency_root/src"
printf '%s\n' \
  '[package]' \
  'name = "bench-dependency"' \
  'version = "0.1.0"' \
  'edition = "2024"' \
  >"$dependency_root/Cargo.toml"
printf 'pub fn unused() {}\n' >"$dependency_root/src/lib.rs"
for ((index = 0; index < packages; index++)); do
  member="$(printf 'member-%04d' "$index")"
  printf 'bench-dependency = { path = "%s" }\n' "$dependency_root" >>"$fixture/crates/$member/Cargo.toml"
done

(cd "$fixture" && cargo generate-lockfile --quiet)
check="cd '$fixture' && CARGO_RAIL_CACHE_DIR='$cache_base' '$binary' rail unify --check --format json >/dev/null; code=\$?; test \$code -eq 0 -o \$code -eq 1"
printf '%s\n' "$check" >"$results/command.txt"

if [[ "${CARGO_RAIL_BENCH_COLD_COMPILE:-0}" == "1" ]]; then
  hyperfine --runs "$runs" --prepare "rm -rf '$fixture/target'" \
    --export-json "$results/cold-compilation.json" "$check"
else
  printf '{"results":[{"times":[]}]}\n' >"$results/cold-compilation.json"
fi

# Cargo's ordinary target artifacts stay warm in all three scenarios. "cold"
# means a cold private local CAS; Cargo-Rail's command-scoped acquisition
# artifacts are intentionally not treated as durable evidence.
hyperfine --warmup 1 --runs "$runs" --prepare "rm -rf '$cache_base/cargo-rail'" \
  --export-json "$results/semantic-cold.json" "$check"

sh -c "$check"
hyperfine --warmup 1 --runs "$runs" --export-json "$results/warm.json" "$check"

changed="$fixture/crates/member-0000/src/lib.rs"
hyperfine --warmup 1 --runs "$runs" --prepare "touch '$changed'" \
  --export-json "$results/one-member-change.json" "$check"

real_cargo="$(command -v cargo)"
mkdir -p "$fixture/bench-bin"
printf '#!/usr/bin/env bash\nprintf '\''cargo\\n'\'' >>%q\nexec %q "$@"\n' \
  "$results/subprocesses.log" "$real_cargo" >"$fixture/bench-bin/cargo"
printf '#!/usr/bin/env bash\nprintf '\''rustc\\n'\'' >>%q\nexec "$@"\n' \
  "$results/subprocesses.log" >"$fixture/bench-bin/rustc-wrapper"
chmod +x "$fixture/bench-bin/cargo" "$fixture/bench-bin/rustc-wrapper"
: >"$results/subprocesses.log"
rm -rf "$cache_base/cargo-rail"
(cd "$fixture" && CARGO_RAIL_CACHE_DIR="$cache_base" PATH="$fixture/bench-bin:$PATH" \
  RUSTC_WORKSPACE_WRAPPER="$fixture/bench-bin/rustc-wrapper" \
  "$binary" rail unify --check --format json >/dev/null 2>/dev/null) || [[ $? -eq 1 ]]
cargo_processes="$(grep -c '^cargo$' "$results/subprocesses.log" || true)"
rustc_invocations="$(grep -c '^rustc$' "$results/subprocesses.log" || true)"

rss_file="$results/rss.txt"
if [[ "$(uname -s)" == "Darwin" ]]; then
  (/usr/bin/time -l sh -c "$check") 2>"$rss_file"
  peak_rss_bytes="$(awk '/maximum resident set size/ {print $1}' "$rss_file" | tail -1)"
else
  (/usr/bin/time -v sh -c "$check") 2>"$rss_file"
  peak_rss_kib="$(awk -F: '/Maximum resident set size/ {gsub(/ /, "", $2); print $2}' "$rss_file" | tail -1)"
  peak_rss_bytes="$((peak_rss_kib * 1024))"
fi
target_disk_bytes="$(( $(du -sk "$fixture/target" | awk '{print $1}') * 1024 ))"

run_diagnostic_sample() {
  local lane="$1"
  local diagnostics="$results/$lane-diagnostics.json"
  local stdout="$results/$lane.stdout"
  local stderr="$results/$lane.stderr"
  local status
  set +e
  (
    cd "$fixture"
    CARGO_RAIL_CACHE_DIR="$cache_base" "$binary" rail \
      --diagnostics-file "$diagnostics" unify --check --format json >"$stdout" 2>"$stderr"
  )
  status=$?
  set -e
  if [[ $status -ne 0 && $status -ne 1 ]]; then
    echo "unify $lane diagnostic sample failed with status $status" >&2
    exit "$status"
  fi
  printf '%s\n' "$status" >"$results/$lane.status"
}

rm -rf "$cache_base/cargo-rail"
run_diagnostic_sample semantic-cold
run_diagnostic_sample warm
touch "$changed"
run_diagnostic_sample one-member-change

jq -e '
  .schema_version == 15
  and .compiler_acquisition.plans == 1
  and .compiler_acquisition.views > 0
  and .compiler_acquisition.cargo_views > 0
  and .compiler_acquisition.compiler_actions > 0
' "$results/semantic-cold-diagnostics.json" >/dev/null
jq -e '
  .schema_version == 15
  and .compiler_acquisition.plans == 1
  and .compiler_acquisition.views > 0
  and .compiler_acquisition.cargo_views == 0
  and .compiler_acquisition.compiler_actions == 0
  and .compiler_acquisition.evidence_cache_hits == .compiler_acquisition.views
' "$results/warm-diagnostics.json" >/dev/null

jq -S 'del(.evidence_cache)' "$results/semantic-cold.stdout" >"$results/semantic-cold.normalized.json"
jq -S 'del(.evidence_cache)' "$results/warm.stdout" >"$results/warm.normalized.json"
cmp "$results/semantic-cold.normalized.json" "$results/warm.normalized.json"

if [[ "$(uname -s)" == "Darwin" ]]; then
  user_cpu_seconds="$(awk '/ real .* user .* sys/ {print $3}' "$rss_file" | tail -1)"
  system_cpu_seconds="$(awk '/ real .* user .* sys/ {print $5}' "$rss_file" | tail -1)"
else
  user_cpu_seconds="$(awk -F: '/User time \(seconds\)/ {gsub(/ /, "", $2); print $2}' "$rss_file" | tail -1)"
  system_cpu_seconds="$(awk -F: '/System time \(seconds\)/ {gsub(/ /, "", $2); print $2}' "$rss_file" | tail -1)"
fi

jq -n \
  --argjson packages "$packages" \
  --argjson declarations "$packages" \
  --argjson runs "$runs" \
  --argjson cargo_processes "$cargo_processes" \
  --argjson rustc_invocations "$rustc_invocations" \
  --argjson peak_rss_bytes "${peak_rss_bytes:-0}" \
  --argjson target_disk_bytes "$target_disk_bytes" \
  --argjson user_cpu_seconds "${user_cpu_seconds:-0}" \
  --argjson system_cpu_seconds "${system_cpu_seconds:-0}" \
  --slurpfile environment "$results/environment.json" \
  --slurpfile cold_compile "$results/cold-compilation.json" \
  --slurpfile cold "$results/semantic-cold.json" \
  --slurpfile warm "$results/warm.json" \
  --slurpfile incremental "$results/one-member-change.json" \
  --slurpfile acquisition_cold "$results/semantic-cold-diagnostics.json" \
  --slurpfile acquisition_warm "$results/warm-diagnostics.json" \
  --slurpfile acquisition_incremental "$results/one-member-change-diagnostics.json" '
  def stats($input):
    ($input[0].results[0].times | sort) as $times |
    if ($times | length) == 0 then null else {
      p50_seconds: $times[((($times | length) * 0.50) | floor)],
      p95_seconds: $times[((((($times | length) * 0.95) | ceil) - 1) | if . < 0 then 0 else . end)],
      min_seconds: ($times | min),
      max_seconds: ($times | max)
    } end;
  {
    schema_version: 2,
    environment: $environment[0],
    packages: $packages,
    direct_declarations: $declarations,
    runs: $runs,
    cold_compilation: stats($cold_compile),
    semantic_cold: stats($cold),
    warm: stats($warm),
    one_member_change: stats($incremental),
    peak_rss_bytes: $peak_rss_bytes,
    user_cpu_seconds: $user_cpu_seconds,
    system_cpu_seconds: $system_cpu_seconds,
    target_disk_bytes: $target_disk_bytes,
    cargo_processes: $cargo_processes,
    rustc_invocations: $rustc_invocations,
    compiler_acquisition: {
      semantic_cold: $acquisition_cold[0].compiler_acquisition,
      warm: $acquisition_warm[0].compiler_acquisition,
      one_member_change: $acquisition_incremental[0].compiler_acquisition,
      warm_zero_work: (
        $acquisition_warm[0].compiler_acquisition.cargo_views == 0
        and $acquisition_warm[0].compiler_acquisition.compiler_actions == 0
      )
    }
  }' | tee "$results/summary.json"

echo "benchmark results: $results" >&2
