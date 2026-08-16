#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
measure="$repo_root/scripts/bench/measure-command.py"

usage() {
  echo "usage: $0 <run-id> <remote-url> [resume-outage]" >&2
  exit 2
}

run_id="${1:-}"
remote_url="${2:-}"
resume="${3:-}"
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
[[ "$remote_url" == s3://* || "$remote_url" == azure://* || "$remote_url" == r2://* ]] || usage
[[ "$#" -eq 2 || ("$#" -eq 3 && "$resume" == resume-outage) ]] || usage

for tool in cargo git jq python3 sandbox-exec; do
  command -v "$tool" >/dev/null || {
    echo "remote fault qualification requires $tool" >&2
    exit 2
  }
done
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  echo "remote fault qualification requires shasum or sha256sum" >&2
  exit 2
fi
[[ "$(uname -s)" == Darwin ]] || {
  echo "remote fault qualification requires macOS sandbox-exec outage isolation" >&2
  exit 2
}

binary="${CARGO_RAIL_BIN:-$repo_root/target/release/cargo-rail}"
if [[ -z "${CARGO_RAIL_BIN+x}" ]]; then
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
fi
[[ -x "$binary" ]] || {
  echo "remote fault qualification binary is not executable: $binary" >&2
  exit 2
}
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"

normalized="$($binary rail cache normalize "$remote_url" --mode read -f json)"
jq -e '(.remote.provider == "aws-s3" or .remote.provider == "azure-blob" or .remote.provider == "cloudflare-r2")
  and .remote.activation == "direct_transport_selected"' \
  <<<"$normalized" >/dev/null || {
  echo "remote fault qualification requires a normalized AWS, Azure, or R2 authority" >&2
  exit 2
}

remote_provider="$(jq -r '.remote.provider' <<<"$normalized")"
normalized_url="$(jq -r '.normalized_url' <<<"$normalized")"
location="${normalized_url%%\?*}"
authority="${location#*://}"
account=""
bucket=""
container=""
endpoint=""
region=""
case "$remote_provider" in
  aws-s3)
    provider_slug=s3
    bucket="${authority%%/*}"
    prefix="${authority#*/}"
    query="${normalized_url#*\?}"
    while IFS='=' read -r name value; do
      [[ "$name" == region ]] && region="$value"
    done < <(printf '%s\n' "$query" | tr '&' '\n')
    [[ "$bucket" != "$authority" && "$region" =~ ^[a-z0-9-]+$ ]] || usage
    command -v aws >/dev/null || {
      echo "AWS S3 fault qualification requires aws" >&2
      exit 2
    }
    ;;
  azure-blob)
    provider_slug=azure
    account="${authority%%/*}"
    remainder="${authority#*/}"
    container="${remainder%%/*}"
    prefix="${remainder#*/}"
    [[ "$account" =~ ^[a-z0-9]{3,24}$ && "$container" != "$remainder" ]] || usage
    [[ "$run_id" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || {
      echo "Azure Blob fault qualification requires a lowercase DNS-safe run ID" >&2
      exit 2
    }
    expected_container="cargo-rail-task9-$run_id"
    [[ "${#expected_container}" -le 63 && "$container" == "$expected_container" ]] || {
      echo "Azure Blob fault qualification requires disposable container $expected_container" >&2
      exit 2
    }
    command -v az >/dev/null || {
      echo "Azure Blob fault qualification requires az" >&2
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
    endpoint="https://$account.r2.cloudflarestorage.com"
    command -v aws >/dev/null || {
      echo "Cloudflare R2 fault qualification requires aws" >&2
      exit 2
    }
    ;;
  *) usage ;;
esac
expected_prefix="cache/cargo-rail/$provider_slug/v3/task9/$run_id"
[[ "$prefix" == "$expected_prefix" ]] || {
  echo "remote fault qualification requires exact disposable prefix $expected_prefix" >&2
  exit 2
}
corrupt_label="$provider_slug-corrupt"
absent_label="$provider_slug-absent"
outage_label="$provider_slug-network-outage"

state="$repo_root/benchmark_results/native-cache-s3-state/$run_id"
consumer="$repo_root/benchmark_results/native-cache/$run_id-consumer"
results="$repo_root/benchmark_results/native-cache/$run_id-faults"
root="$state/fixtures/check"
cargo_home="$state/cargo-homes/check"
baseline="$consumer/raw/check/consumer/outputs.sha256"
[[ -d "$root" && -d "$cargo_home" && -s "$baseline" ]] || {
  echo "remote fault qualification requires retained consumer state and output evidence for $run_id" >&2
  exit 2
}
if [[ "$resume" == resume-outage ]]; then
  [[ -d "$results" \
    && -s "$results/$corrupt_label/events.json" \
    && -s "$results/$corrupt_label/outputs.sha256" \
    && -s "$results/$absent_label/events.json" \
    && -s "$results/$absent_label/outputs.sha256" ]] || {
    echo "remote outage resume requires retained passing corruption and absence evidence" >&2
    exit 2
  }
  [[ ! -e "$results/result.json" ]] || {
    echo "remote fault qualification is already complete: $results" >&2
    exit 2
  }
else
  [[ ! -e "$results" ]] || {
    echo "remote fault qualification result already exists: $results" >&2
    exit 2
  }
fi

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
      echo "remote fault evidence cannot capture non-file untracked input: $untracked" >&2
      return 1
    }
    local status=0
    git -C "$repo_root" diff --binary --no-index -- /dev/null "$untracked" >>"$output" || status=$?
    [[ "$status" -eq 1 ]] || {
      echo "remote fault evidence could not capture untracked input: $untracked" >&2
      return 1
    }
  done < <(git -C "$repo_root" ls-files --others --exclude-standard -z)
}

consumer_environment="$consumer/environment.json"
consumer_result="$consumer/result.json"
jq -e \
  --arg provider "$remote_provider" \
  --arg authority "$(jq -r '.remote.authority' <<<"$normalized")" '
    .schema_version == 2
    and .phase == "consumer"
    and .remote.provider == $provider
    and .remote.authority == $authority
    and .remote.mode == "read"
  ' "$consumer_environment" >/dev/null || {
  echo "remote fault qualification authority disagrees with retained consumer evidence" >&2
  exit 2
}
jq -e '.schema_version == 2 and .phase == "consumer" and .passed' "$consumer_result" >/dev/null || {
  echo "remote fault qualification requires a passing retained consumer result" >&2
  exit 2
}

validation_tmp="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-task9-fault.XXXXXX")"
cleanup_validation_tmp() {
  rm -rf -- "$validation_tmp"
}
trap cleanup_validation_tmp EXIT
git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$validation_tmp/worktree-status.txt"
capture_worktree_patch "$validation_tmp/worktree.diff"
[[ "$(git -C "$repo_root" rev-parse HEAD)" == "$(jq -r '.repository_commit' "$consumer_environment")" \
  && "$(sha256_file "$validation_tmp/worktree.diff")" == "$(jq -r '.worktree_diff_sha256' "$consumer_environment")" \
  && "$(sha256_file "$validation_tmp/worktree-status.txt")" == "$(jq -r '.worktree_status_sha256' "$consumer_environment")" \
  && "$(sha256_file "$binary")" == "$(jq -r '.release_binary_sha256' "$consumer_environment")" \
  && "$(rustc -vV)" == "$(jq -r '.rustc' "$consumer_environment")" \
  && "$(cargo -Vv)" == "$(jq -r '.cargo' "$consumer_environment")" ]] || {
  echo "remote fault qualification inputs drifted from retained consumer evidence" >&2
  exit 2
}
if [[ "$resume" != resume-outage ]]; then
  mkdir -p "$results"
  chmod 700 "$results"
fi

provider_list() {
  case "$remote_provider" in
    aws-s3) aws s3api list-objects-v2 --bucket "$bucket" --prefix "$prefix/" --max-keys 1 --region "$region" ;;
    azure-blob)
      az storage blob list --account-name "$account" --container-name "$container" --prefix "$prefix/" \
        --num-results 1 --auth-mode login --only-show-errors --output json
      ;;
    cloudflare-r2)
      aws s3api list-objects-v2 --bucket "$bucket" --prefix "$prefix/" --max-keys 1 \
        --endpoint-url "$endpoint" --region auto
      ;;
  esac
}

provider_head() {
  local key="$1"
  case "$remote_provider" in
    aws-s3) aws s3api head-object --bucket "$bucket" --key "$key" --region "$region" ;;
    azure-blob)
      az storage blob show --account-name "$account" --container-name "$container" --name "$key" \
        --auth-mode login --only-show-errors --output json
      ;;
    cloudflare-r2)
      aws s3api head-object --bucket "$bucket" --key "$key" --endpoint-url "$endpoint" --region auto
      ;;
  esac
}

provider_corrupt() {
  local key="$1"
  case "$remote_provider" in
    aws-s3)
      aws s3api put-object --bucket "$bucket" --key "$key" --body "$repo_root/Cargo.toml" \
        --content-type application/octet-stream --region "$region"
      ;;
    azure-blob)
      az storage blob upload --account-name "$account" --container-name "$container" --name "$key" \
        --file "$repo_root/Cargo.toml" --overwrite true --content-type application/octet-stream \
        --auth-mode login --no-progress --only-show-errors --output json
      ;;
    cloudflare-r2)
      aws s3api put-object --bucket "$bucket" --key "$key" --body "$repo_root/Cargo.toml" \
        --content-type application/octet-stream --endpoint-url "$endpoint" --region auto
      ;;
  esac
}

provider_delete() {
  local key="$1"
  case "$remote_provider" in
    aws-s3) aws s3api delete-object --bucket "$bucket" --key "$key" --region "$region" ;;
    azure-blob)
      az storage blob delete --account-name "$account" --container-name "$container" --name "$key" \
        --auth-mode login --only-show-errors --output json
      ;;
    cloudflare-r2)
      aws s3api delete-object --bucket "$bucket" --key "$key" --endpoint-url "$endpoint" --region auto
      ;;
  esac
}

manifest_outputs() {
  local cargo_output="$1"
  local output="$2"
  local target="$root/target"
  : >"$output"
  while IFS= read -r path; do
    [[ "$path" == "$target/"* && -f "$path" && ! -L "$path" ]] || {
      echo "remote fault output is not one target-contained regular file: $path" >&2
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
      absent: ([.[] | select(
        .status == "miss" and (.reason | startswith("remote_entry_not_found;"))
      )] | length),
      unavailable: ([.[] | select(
        .status == "miss" and (.reason | startswith("remote_cache_unavailable;"))
      )] | length),
      remote_request_attempts: ([.[].remote_request_attempts] | add // 0),
      remote_coordinator_requests: ([.[].remote_coordinator_requests] | add // 0),
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
    echo "remote fault qualification found non-fresh local state for $label" >&2
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
    echo "$remote_provider $label cold fallback changed exact outputs" >&2
    exit 1
  }
}

if [[ "$resume" != resume-outage ]]; then
  provider_list >"$results/provider-list-before.json"
  base_action_key="$(
    find "$consumer/raw/check/consumer/events" -type f -name 'event-*.json' -print0 |
      xargs -0 jq -r 'select(.remote_base_action_key != null) | .remote_base_action_key' |
      head -n 1
  )"
  [[ "$base_action_key" =~ ^compiler-base-v10-sha256-[0-9a-f]{64}$ ]] || {
    echo "remote fault qualification found no verified remote base action" >&2
    exit 2
  }
  digest="${base_action_key##*-}"
  corrupt_key="$prefix/native-v5/entries/${digest:0:2}/$base_action_key"
  provider_head "$corrupt_key" >"$results/corrupt-object-before.json"
  provider_corrupt "$corrupt_key" >"$results/corrupt-object-after.json"
  printf '%s\n' "$corrupt_key" >"$results/corrupt-object-key.txt"

  run_check "$corrupt_label" available
  jq -e '
    .events > 0
    and .hits > 0
    and .rejected > 0
    and .remote_payload_bytes_read > 0
    and .remote_payload_bytes_written == 0
  ' "$results/$corrupt_label/events.json" >/dev/null || {
    echo "$remote_provider corruption did not reject the object and compile cold without a remote write" >&2
    exit 1
  }

  provider_delete "$corrupt_key" >"$results/absent-object-delete.json"
  if provider_head "$corrupt_key" >"$results/absent-object-after-delete.json" \
    2>"$results/absent-object-after-delete.stderr"; then
    echo "$remote_provider deletion left the selected remote object readable" >&2
    exit 1
  fi
  run_check "$absent_label" available
  jq -e '
    .events > 0
    and .hits > 0
    and .misses > 0
    and .absent > 0
    and (.remote_request_attempts > 0 or .remote_coordinator_requests > 0)
    and .remote_payload_bytes_written == 0
    and (.remote_errors | length) == 0
  ' "$results/$absent_label/events.json" >/dev/null || {
    echo "$remote_provider absence did not compile cold without a remote write" >&2
    exit 1
  }
fi

if sandbox-exec -p '(version 1)(allow default)(deny network-outbound (remote ip))' \
  python3 -c 'import socket; socket.create_connection(("example.com", 443), 2)' >/dev/null 2>&1; then
  echo "remote outage sandbox did not deny outbound provider traffic" >&2
  exit 2
fi
if [[ ! -s "$results/$outage_label/events.json" || ! -s "$results/$outage_label/outputs.sha256" ]]; then
  run_check "$outage_label" denied
fi
jq -e '
  .events > 0
  and .misses > 0
  and .unavailable > 0
  and .remote_payload_bytes_written == 0
' "$results/$outage_label/events.json" >/dev/null || {
  echo "$remote_provider network outage did not compile cold without a remote write" >&2
  exit 1
}

git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$results/worktree-status.txt"
capture_worktree_patch "$results/worktree.diff"
printf '%s  %s\n' \
  "$(sha256_file "$repo_root/scripts/ci/qualify-native-cache-remote-faults.sh")" \
  scripts/ci/qualify-native-cache-remote-faults.sh >"$results/harness-sha256.txt"
printf '%s  %s\n' "$(sha256_file "$measure")" scripts/bench/measure-command.py >>"$results/harness-sha256.txt"

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg run_id "$run_id" \
  --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
  --arg worktree_diff_sha256 "$(sha256_file "$results/worktree.diff")" \
  --arg worktree_status_sha256 "$(sha256_file "$results/worktree-status.txt")" \
  --arg binary_sha256 "$(sha256_file "$binary")" \
  --arg harness_sha256 "$(sha256_file "$results/harness-sha256.txt")" \
  --arg consumer_environment_sha256 "$(sha256_file "$consumer_environment")" \
  --arg consumer_result_sha256 "$(sha256_file "$consumer_result")" \
  --arg consumer_output_manifest_sha256 "$(sha256_file "$baseline")" \
  --arg rustc "$(rustc -vV)" \
  --arg cargo "$(cargo -Vv)" \
  --arg host "$(uname -a)" \
  --arg provider "$remote_provider" \
  --arg remote_authority "$(jq -r '.remote.authority' <<<"$normalized")" '
  {
    schema_version: 1,
    generated_at: $generated_at,
    run_id: $run_id,
    repository_commit: $commit,
    worktree_diff_sha256: $worktree_diff_sha256,
    worktree_status_sha256: $worktree_status_sha256,
    release_binary_sha256: $binary_sha256,
    benchmark_harness_sha256: $harness_sha256,
    consumer_environment_sha256: $consumer_environment_sha256,
    consumer_result_sha256: $consumer_result_sha256,
    consumer_output_manifest_sha256: $consumer_output_manifest_sha256,
    rustc: $rustc,
    cargo: $cargo,
    host: $host,
    remote: {provider: $provider, authority: $remote_authority, mode: "read"}
  }
' >"$results/environment.json"

jq -n \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg run_id "$run_id" \
  --arg provider "$remote_provider" \
  --arg remote_authority "$(jq -r '.remote.authority' <<<"$normalized")" \
  --slurpfile corrupt "$results/$corrupt_label/events.json" \
  --slurpfile absent "$results/$absent_label/events.json" \
  --slurpfile outage "$results/$outage_label/events.json" \
  '{
    schema_version: 3,
    generated_at: $generated_at,
    run_id: $run_id,
    remote: {provider: $provider, authority: $remote_authority},
    corruption: $corrupt[0],
    absence: $absent[0],
    outage: $outage[0],
    exact_outputs_preserved: true,
    passed: true
  }' >"$results/result.json"

echo "remote fault qualification result: $results"
