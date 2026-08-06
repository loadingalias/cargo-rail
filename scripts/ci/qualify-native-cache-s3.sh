#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mode="${1:-}"
run_id="${2:-}"

case "$mode" in
  publish | hit) ;;
  *)
    echo "usage: $0 <publish|hit> <run-id>" >&2
    exit 2
    ;;
esac
if [[ ! "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ || "$run_id" == *..* ]]; then
  echo "native-cache S3 qualification run ID is invalid" >&2
  exit 2
fi

alias="${CARGO_RAIL_CACHE_ALIAS:-qualification_rw}"
region="${CARGO_RAIL_CACHE_REGION:?set CARGO_RAIL_CACHE_REGION}"
owner="${CARGO_RAIL_CACHE_EXPECTED_OWNER:?set CARGO_RAIL_CACHE_EXPECTED_OWNER}"
bucket="${CARGO_RAIL_CACHE_BUCKET:?set CARGO_RAIL_CACHE_BUCKET}"
prefix="${CARGO_RAIL_CACHE_PREFIX:?set CARGO_RAIL_CACHE_PREFIX}"
target_map="${CARGO_RAIL_CACHE_TARGETS_FILE:-$HOME/.config/cargo-rail/cache-targets.json}"
results="$repo_root/benchmark_results/native-cache/$run_id"
binary="$repo_root/target/release/cargo-rail"
wrapper="$repo_root/target/release/cargo-rail-native-rustc-wrapper"
toolchain="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$repo_root/rust-toolchain.toml" | head -1)"

[[ "$alias" =~ ^[a-z][a-z0-9_-]{0,63}$ ]] || {
  echo "native-cache S3 qualification alias is invalid" >&2
  exit 2
}
[[ "$region" =~ ^[a-z0-9][a-z0-9-]*[a-z0-9]$ ]] || {
  echo "native-cache S3 qualification region is invalid" >&2
  exit 2
}
[[ "$owner" =~ ^[0-9]{12}$ && -n "$bucket" && -n "$prefix" && -n "$toolchain" ]] || {
  echo "native-cache S3 qualification authority is invalid" >&2
  exit 2
}
[[ "$target_map" = /* ]] || {
  echo "native-cache S3 qualification target map must be absolute" >&2
  exit 2
}
[[ ! -e "$results" ]] || {
  echo "native-cache S3 qualification result already exists: $results" >&2
  exit 2
}

umask 077
mkdir -p "$(dirname "$target_map")" "$results"
target_map_staging="${target_map}.staging.$$"
trap 'rm -f -- "${target_map_staging:-}"; rm -rf -- "${fixture_root:-}"' EXIT
jq -n \
  --arg alias "$alias" \
  --arg region "$region" \
  --arg owner "$owner" \
  --arg bucket "$bucket" \
  --arg prefix "$prefix" \
  '{
    version: 1,
    targets: {
      ($alias): {
        protocol: "s3",
        region: $region,
        expected_bucket_owner: $owner,
        bucket: $bucket,
        prefix: $prefix,
        role: "read_write",
        shareable_environment: ["CARGO_PKG_VERSION"]
      }
    }
  }' >"$target_map_staging"
chmod 600 "$target_map_staging"
mv -f "$target_map_staging" "$target_map"
target_map_staging=""

cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
[[ -x "$binary" && -x "$wrapper" ]] || {
  echo "native-cache S3 qualification binaries are unavailable" >&2
  exit 1
}

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-s3-qualification.XXXXXX")"
fixture="$fixture_root/workspace"
"$repo_root/scripts/fixtures/materialize-native-cache.sh" "$fixture" "$fixture_root/git-source"
printf '[cache]\nl2 = "%s"\n' "$alias" >"$fixture/.config/rail.toml"

diagnostics="$results/diagnostics.json"
stdout="$results/stdout"
stderr="$results/stderr"
(
  cd "$fixture"
  env \
    -u CARGO_INCREMENTAL \
    -u RUSTC_WRAPPER \
    -u RUSTC_WORKSPACE_WRAPPER \
    RUSTUP_TOOLCHAIN="$toolchain" \
    CARGO_RAIL_CACHE_TARGETS_FILE="$target_map" \
    CARGO_RAIL_CACHE_DIR="$fixture_root/l1" \
    "$binary" rail --diagnostics-file "$diagnostics" run --all --action build --explain -- \
      --locked \
      --offline \
      --message-format=json-render-diagnostics \
      --no-default-features \
      --exclude fixture-api \
      --exclude fixture-build \
      --exclude fixture-cli \
      --exclude fixture-macros \
      --exclude fixture-native \
      --exclude fixture-native-sys \
      --exclude fixture-service-a \
      --exclude fixture-service-b \
      --exclude fixture-storage \
      >"$stdout" 2>"$stderr"
)

native_summary="$(grep -F "action \`build\` native compiler cache:" "$stdout")"
remote_summary="$(grep -F "action \`build\` remote compiler cache:" "$stdout")"
case "$mode" in
  publish)
    [[ "$native_summary" == *"hits=0 misses=1"* && "$remote_summary" == *"hits=0 misses=1"* ]]
    [[ "$remote_summary" == *"conflicts=0 failures=0 publications=1"* ]]
    expected_outcome=miss
    ;;
  hit)
    [[ "$native_summary" == *"hits=1 misses=0"* && "$remote_summary" == *"hits=2 misses=0"* ]]
    [[ "$remote_summary" == *"conflicts=0 failures=0 publications=0"* ]]
    expected_outcome=hit
    ;;
esac

action_identity="$(
  jq -er --arg outcome "$expected_outcome" '
    [.native_cache_wrapper.events[] | select(.outcome == $outcome) | .unit_identity]
    | if length == 1 then .[0] else error("expected one qualified compiler unit") end
  ' "$diagnostics"
)"
output="$(find "$fixture/target" -type f -name 'libfixture_types-*.rmeta' -print)"
[[ -f "$output" && "$(printf '%s\n' "$output" | wc -l | tr -d ' ')" -eq 1 ]] || {
  echo "native-cache S3 qualification output is ambiguous" >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  output_sha256="$(sha256sum "$output" | awk '{print $1}')"
else
  output_sha256="$(shasum -a 256 "$output" | awk '{print $1}')"
fi

jq -n \
  --arg mode "$mode" \
  --arg run_id "$run_id" \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg host "$(uname -a)" \
  --arg rustc "$(RUSTUP_TOOLCHAIN="$toolchain" rustc -vV)" \
  --arg action_identity "$action_identity" \
  --arg output_sha256 "sha256:$output_sha256" \
  --arg native_summary "$native_summary" \
  --arg remote_summary "$remote_summary" \
  '{
    schema_version: 1,
    mode: $mode,
    run_id: $run_id,
    generated_at: $generated_at,
    host: $host,
    rustc: $rustc,
    action_identity: $action_identity,
    output_sha256: $output_sha256,
    native_summary: $native_summary,
    remote_summary: $remote_summary
  }' >"$results/run.json"

printf '%s\n%s\n' "$native_summary" "$remote_summary"
echo "native-cache S3 qualification result: $results" >&2
