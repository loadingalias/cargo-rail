#!/usr/bin/env bash
set -euo pipefail

# Runs on the remote qualification machine itself (invoked via
# `just bench-native-cache-remote`, which the local orchestrator in
# remote-native-cache.sh drives over SSH). Tags the run with the instance
# type and a fixed results directory, then dispatches to native-cache.sh.

usage() {
  echo "usage: $0 <smoke|run> <runs> <run-id>" >&2
  echo "       $0 l2 1 <run-id> <alias> <region> <owner> <bucket> <prefix>" >&2
  exit 2
}

mode="${1:-}"
runs="${2:-}"
run_id="${3:-}"

case "$mode" in
  smoke | run | l2) ;;
  *) usage ;;
esac
[[ -n "$runs" && -n "$run_id" ]] || usage
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

export CARGO_RAIL_BENCH_INSTANCE_TYPE="${DEV_MACHINE_INSTANCE_TYPE:-unknown}"
export CARGO_RAIL_BENCH_RESULTS="$PWD/benchmark_results/native-cache/$run_id"

if [[ "$mode" == smoke ]]; then
  exec "$script_dir/native-cache.sh" smoke
elif [[ "$mode" == run ]]; then
  exec "$script_dir/native-cache.sh" run "$runs"
fi

[[ "$runs" == 1 && "$#" -eq 8 ]] || usage
alias="$4"
region="$5"
owner="$6"
bucket="$7"
prefix="$8"
[[ "$alias" =~ ^[a-z][a-z0-9_-]{0,63}$ ]] || usage
[[ "$region" =~ ^[a-z0-9][a-z0-9-]*[a-z0-9]$ && "$owner" =~ ^[0-9]{12}$ ]] || usage
[[ -n "$bucket" && -n "$prefix" ]] || usage

umask 077
target_map="$HOME/.config/cargo-rail/native-cache-bench-$run_id.json"
target_map_staging="${target_map}.staging.$$"
mkdir -p "$(dirname "$target_map")"
cleanup() {
  rm -f -- "$target_map_staging" "$target_map"
}
trap cleanup EXIT
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
mv "$target_map_staging" "$target_map"

export CARGO_RAIL_BENCH_L2_ALIAS="$alias"
export CARGO_RAIL_BENCH_L2_TARGETS_FILE="$target_map"
"$script_dir/native-cache.sh" run 1
