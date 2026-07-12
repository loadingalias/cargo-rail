#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
packages="${1:-25}"
runs="${2:-10}"
binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
results="${CARGO_RAIL_BENCH_RESULTS:-$repo_root/target/benchmarks/unify/$(date -u +%Y%m%dT%H%M%SZ)}"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-unify-bench.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

for tool in hyperfine jq cargo; do
  command -v "$tool" >/dev/null || { echo "missing required benchmark tool: $tool" >&2; exit 2; }
done
[[ -x "$binary" ]] || { echo "build the release binary first: cargo build --release" >&2; exit 2; }
mkdir -p "$results" "$fixture/.config" "$fixture/crates"
results="$(cd "$(dirname "$results")" && pwd)/$(basename "$results")"

{
  printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n\n[workspace.package]\nedition = "2024"\nrust-version = "1.95"\n'
} >"$fixture/Cargo.toml"
printf '[workspace]\nroot = "."\n\n[unify]\ndetect_unused = true\ncompiler_diag_cache = true\n' >"$fixture/.config/rail.toml"

dependencies=(anyhow log once_cell regex semver serde serde_json tempfile thiserror toml_edit)
for ((index = 0; index < packages; index++)); do
  member="$(printf 'member-%04d' "$index")"
  mkdir -p "$fixture/crates/$member/src"
  {
    printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition.workspace = true\nrust-version.workspace = true\n\n[dependencies]\n' "$member"
    for dependency in "${dependencies[@]}"; do
      printf '%s = "*"\n' "$dependency"
    done
  } >"$fixture/crates/$member/Cargo.toml"
  printf 'pub fn member_%04d() -> usize { %d }\n' "$index" "$index" >"$fixture/crates/$member/src/lib.rs"
done

(cd "$fixture" && cargo generate-lockfile --quiet)
check="cd '$fixture' && '$binary' rail unify --check --format json >/dev/null; code=\$?; test \$code -eq 0 -o \$code -eq 1"

if [[ "${CARGO_RAIL_BENCH_COLD_COMPILE:-0}" == "1" ]]; then
  hyperfine --runs "$runs" --prepare "rm -rf '$fixture/target'" \
    --export-json "$results/cold-compilation.json" "$check"
else
  printf '{"results":[{"times":[]}]}\n' >"$results/cold-compilation.json"
fi

# Cargo artifacts stay warm in all three scenarios. "cold" means a cold
# cargo-rail semantic evidence cache, which isolates the cache's contribution.
hyperfine --warmup 1 --runs "$runs" --prepare "rm -rf '$fixture/target/cargo-rail/cache'" \
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
rm -rf "$fixture/target/cargo-rail/cache"
(cd "$fixture" && PATH="$fixture/bench-bin:$PATH" RUSTC_WORKSPACE_WRAPPER="$fixture/bench-bin/rustc-wrapper" \
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

jq -n \
  --argjson packages "$packages" \
  --argjson declarations "$((packages * ${#dependencies[@]}))" \
  --argjson runs "$runs" \
  --argjson cargo_processes "$cargo_processes" \
  --argjson rustc_invocations "$rustc_invocations" \
  --argjson peak_rss_bytes "${peak_rss_bytes:-0}" \
  --argjson target_disk_bytes "$target_disk_bytes" \
  --slurpfile cold_compile "$results/cold-compilation.json" \
  --slurpfile cold "$results/semantic-cold.json" \
  --slurpfile warm "$results/warm.json" \
  --slurpfile incremental "$results/one-member-change.json" '
  def stats($input):
    ($input[0].results[0].times | sort) as $times |
    if ($times | length) == 0 then null else {
      p50_seconds: $times[((($times | length) * 0.50) | floor)],
      p95_seconds: $times[((((($times | length) * 0.95) | ceil) - 1) | if . < 0 then 0 else . end)],
      min_seconds: ($times | min),
      max_seconds: ($times | max)
    } end;
  {
    schema_version: 1,
    packages: $packages,
    direct_declarations: $declarations,
    runs: $runs,
    cold_compilation: stats($cold_compile),
    semantic_cold: stats($cold),
    warm: stats($warm),
    one_member_change: stats($incremental),
    peak_rss_bytes: $peak_rss_bytes,
    target_disk_bytes: $target_disk_bytes,
    cargo_processes: $cargo_processes,
    rustc_invocations: $rustc_invocations
  }' | tee "$results/summary.json"

echo "benchmark results: $results" >&2
