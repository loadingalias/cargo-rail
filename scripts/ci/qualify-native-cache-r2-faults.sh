#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
measure="$repo_root/scripts/bench/measure-command.py"

usage() {
  echo "usage: $0 <run-id> <r2-remote-url> <bucket> [resume-outage]" >&2
  exit 2
}

run_id="${1:-}"
remote_url="${2:-}"
bucket="${3:-}"
resume="${4:-}"
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
[[ "$remote_url" == r2://* && -n "$bucket" ]] || usage
[[ "$#" -eq 3 || ("$#" -eq 4 && "$resume" == resume-outage) ]] || usage

for tool in aws cargo jq python3 sandbox-exec shasum; do
  command -v "$tool" >/dev/null || {
    echo "R2 fault qualification requires $tool" >&2
    exit 2
  }
done

binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
[[ -x "$binary" ]] || {
  echo "R2 fault qualification binary is not executable: $binary" >&2
  exit 2
}

normalized="$($binary rail cache normalize "$remote_url" --mode read -f json)"
jq -e '.remote.provider == "cloudflare-r2" and .remote.activation == "direct_transport_selected"' \
  <<<"$normalized" >/dev/null || {
  echo "R2 fault qualification requires a normalized Cloudflare R2 authority" >&2
  exit 2
}

authority="${remote_url#r2://}"
account="${authority%%/*}"
remainder="${authority#*/}"
url_bucket="${remainder%%/*}"
prefix="${remainder#*/}"
[[ "$account" =~ ^[0-9a-f]{32}$ && "$url_bucket" == "$bucket" && "$prefix" != "$remainder" ]] || usage
[[ "$prefix" == cache/cargo-rail/r2/v3/task5/* && "$prefix" != *..* ]] || {
  echo "R2 fault qualification refuses a prefix outside cache/cargo-rail/r2/v3/task5/" >&2
  exit 2
}
endpoint="https://$account.r2.cloudflarestorage.com"

state="$repo_root/benchmark_results/native-cache-s3-state/$run_id"
consumer="$repo_root/benchmark_results/native-cache/$run_id-consumer"
results="$repo_root/benchmark_results/native-cache/$run_id-faults"
root="$state/fixtures/check"
cargo_home="$state/cargo-homes/check"
baseline="$consumer/raw/check/consumer/outputs.sha256"
[[ -d "$root" && -d "$cargo_home" && -s "$baseline" ]] || {
  echo "R2 fault qualification requires retained consumer state and output evidence for $run_id" >&2
  exit 2
}
if [[ "$resume" == resume-outage ]]; then
  [[ -d "$results" && -s "$results/r2-corrupt/events.json" && -s "$results/r2-corrupt/outputs.sha256" ]] || {
    echo "R2 outage resume requires retained passing corruption evidence" >&2
    exit 2
  }
else
  [[ ! -e "$results" ]] || {
    echo "R2 fault qualification result already exists: $results" >&2
    exit 2
  }
  mkdir -p "$results"
  chmod 700 "$results"
fi

aws_r2() {
  aws s3api "$@" --bucket "$bucket" --endpoint-url "$endpoint" --region auto
}

manifest_outputs() {
  local cargo_output="$1"
  local output="$2"
  local target="$root/target"
  : >"$output"
  while IFS= read -r path; do
    [[ "$path" == "$target/"* && -f "$path" && ! -L "$path" ]] || {
      echo "R2 fault output is not one target-contained regular file: $path" >&2
      return 1
    }
    local mode
    mode="$(stat -f '%Lp' "$path")"
    printf '%s  %s  %s\n' "$(shasum -a 256 "$path" | awk '{print $1}')" "$mode" "${path#"$root"/}" >>"$output"
  done < <(
    {
      find "$target" -type f ! -path '*/incremental/*' \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -print
      jq -Rr 'fromjson? | select(.reason == "compiler-artifact") | (.filenames[]?), (.executable // empty)' \
        "$cargo_output"
    } | LC_ALL=C sort -u
  )
}

summarize_events() {
  local directory="$1"
  local output="$2"
  local -a events=()
  mapfile -d '' -t events < <(find "$directory" -type f -name 'event-*.json' -print0 | sort -z)
  jq -s '
    {
      events: length,
      hits: ([.[] | select(.status == "hit")] | length),
      misses: ([.[] | select(.status == "miss")] | length),
      rejected: ([.[] | select(
        .status == "miss" and (.reason | startswith("remote_entry_rejected;"))
      )] | length),
      unavailable: ([.[] | select(
        .status == "miss" and (.reason | startswith("remote_cache_unavailable;"))
      )] | length),
      remote_request_attempts: ([.[].remote_request_attempts] | add // 0),
      remote_payload_bytes_read: ([.[].remote_payload_bytes_read] | add // 0),
      remote_payload_bytes_written: ([.[].remote_payload_bytes_written] | add // 0),
      remote_errors: ([.[].remote_error? | select(. != null)] | unique)
    }' "${events[@]}" >"$output"
}

run_check() {
  local label="$1"
  local network="$2"
  local cache="$state/caches/check-$label"
  local evidence="$results/$label"
  local target="$root/target"
  local previous_target="$root/target.before-$label"
  [[ ! -e "$cache" && ! -e "$evidence" && -d "$target" && ! -e "$previous_target" ]] || {
    echo "R2 fault qualification found non-fresh local state for $label" >&2
    exit 2
  }
  mv "$target" "$previous_target"
  mkdir -p "$cache" "$evidence/events"
  chmod 700 "$cache" "$evidence" "$evidence/events"
  CARGO_HOME="$cargo_home" "$binary" rail cache setup --local-dir "$cache" --max-size 10GiB \
    --remote "$remote_url" --remote-mode read >/dev/null

  local -a command=(cargo check --workspace --all-features --locked --offline --message-format=json-render-diagnostics)
  if [[ "$network" == denied ]]; then
    command=(sandbox-exec -p '(version 1)(allow default)(deny network-outbound (remote ip))' "${command[@]}")
  fi
  python3 "$measure" \
    --cwd "$root" \
    --stdout "$evidence/stdout" \
    --stderr "$evidence/stderr" \
    --output "$evidence/timing.json" \
    --unset RUSTC_WRAPPER \
    --unset CARGO_BUILD_RUSTC_WRAPPER \
    --unset RUSTC_WORKSPACE_WRAPPER \
    --unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
    --unset CARGO_TARGET_DIR \
    --unset RUSTFLAGS \
    --unset CARGO_ENCODED_RUSTFLAGS \
    --env "CARGO_HOME=$cargo_home" \
    --env CARGO_TERM_COLOR=never \
    --env CARGO_NET_OFFLINE=true \
    --env CARGO_INCREMENTAL=0 \
    --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1 \
    --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$evidence/events" \
    -- "${command[@]}"
  manifest_outputs "$evidence/stdout" "$evidence/outputs.sha256"
  summarize_events "$evidence/events" "$evidence/events.json"
  cmp "$baseline" "$evidence/outputs.sha256" || {
    echo "R2 $label cold fallback changed exact outputs" >&2
    exit 1
  }
}

if [[ "$resume" != resume-outage ]]; then
  aws_r2 list-objects-v2 --prefix "$prefix" --max-keys 1 >/dev/null
  base_action_key="$(
    find "$consumer/raw/check/consumer/events" -type f -name 'event-*.json' -print0 |
      xargs -0 jq -r 'select(.remote_base_action_key != null) | .remote_base_action_key' |
      head -n 1
  )"
  [[ "$base_action_key" =~ ^compiler-base-v10-sha256-[0-9a-f]{64}$ ]] || {
    echo "R2 fault qualification found no verified remote base action" >&2
    exit 2
  }
  digest="${base_action_key##*-}"
  corrupt_key="$prefix/native-v5/entries/${digest:0:2}/$base_action_key"
  aws_r2 head-object --key "$corrupt_key" --query '{content_length:ContentLength}' >"$results/corrupt-object-before.json"
  aws_r2 put-object --key "$corrupt_key" --body "$repo_root/Cargo.toml" --content-type application/octet-stream \
    --query '{etag:ETag}' >"$results/corrupt-object-after.json"
  printf '%s\n' "$corrupt_key" >"$results/corrupt-object-key.txt"

  run_check r2-corrupt available
  jq -e '
    .events > 0
    and .hits > 0
    and .rejected > 0
    and .remote_payload_bytes_read > 0
    and .remote_payload_bytes_written == 0
  ' "$results/r2-corrupt/events.json" >/dev/null || {
    echo "R2 corruption did not reject the object and compile cold without a remote write" >&2
    exit 1
  }
fi

if sandbox-exec -p '(version 1)(allow default)(deny network-outbound (remote ip))' \
  python3 -c 'import socket, sys; socket.create_connection((sys.argv[1], 443), 2)' \
  "$account.r2.cloudflarestorage.com" >/dev/null 2>&1; then
  echo "R2 outage sandbox did not deny the provider endpoint" >&2
  exit 2
fi
if [[ ! -s "$results/r2-network-outage/events.json" || ! -s "$results/r2-network-outage/outputs.sha256" ]]; then
  run_check r2-network-outage denied
fi
jq -e '
  .events > 0
  and .misses > 0
  and .unavailable > 0
  and .remote_payload_bytes_written == 0
' "$results/r2-network-outage/events.json" >/dev/null || {
  echo "R2 network outage did not compile cold without a remote write" >&2
  exit 1
}

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg run_id "$run_id" \
  --arg remote_authority "$(jq -r '.remote.authority' <<<"$normalized")" \
  --slurpfile corrupt "$results/r2-corrupt/events.json" \
  --slurpfile outage "$results/r2-network-outage/events.json" \
  '{
    schema_version: 1,
    generated_at: $generated_at,
    run_id: $run_id,
    remote: {provider: "cloudflare-r2", authority: $remote_authority},
    corruption: $corrupt[0],
    outage: $outage[0],
    exact_outputs_preserved: true,
    passed: true
  }' >"$results/result.json"

echo "R2 fault qualification result: $results"
