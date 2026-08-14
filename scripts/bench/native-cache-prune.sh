#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
results_root="$repo_root/benchmark_results"

[[ "$#" -gt 0 ]] || {
  echo "usage: $0 <run-id>..." >&2
  exit 2
}

for run_id in "$@"; do
  if [[ ! "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ || "$run_id" == *..* ]]; then
    echo "invalid native-cache run ID: $run_id" >&2
    exit 2
  fi
done

for run_id in "$@"; do
  run="$results_root/native-cache/$run_id"
  s3_state="$results_root/native-cache-s3-state/$run_id"
  s3_performance_state="$results_root/native-cache-s3-performance-state/$run_id"
  archive="$results_root/.transfers/$run_id.tar"
  manifest="$archive.sha256"
  staging="$results_root/.transfers/.$run_id.tar.staging"
  manifest_staging="$results_root/.transfers/.$run_id.tar.sha256.staging"
  rm -rf -- "$run" "$s3_state" "$s3_performance_state"
  rm -f -- "$archive" "$manifest" "$staging" "$manifest_staging"
  echo "native-cache prune: removed $run_id" >&2
done
