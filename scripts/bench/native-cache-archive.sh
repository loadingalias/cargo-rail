#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
run_id="${1:-}"

if [[ ! "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ || "$run_id" == *..* ]]; then
  echo "usage: $0 <run-id>" >&2
  exit 2
fi

results_root="$repo_root/benchmark_results"
run="$results_root/native-cache/$run_id"
transfer_root="$results_root/.transfers"
archive="$transfer_root/$run_id.tar"
manifest="$archive.sha256"
archive_staging="$transfer_root/.$run_id.tar.staging"
manifest_staging="$transfer_root/.$run_id.tar.sha256.staging"

[[ -d "$run" ]] || {
  echo "native-cache run does not exist: $run" >&2
  exit 2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

if [[ -f "$archive" && -f "$manifest" ]]; then
  read -r expected_digest expected_name <"$manifest"
  if [[ "$expected_name" == "$run_id.tar" && "$(sha256_file "$archive")" == "$expected_digest" ]]; then
    echo "native-cache archive: keeping complete $run_id" >&2
    exit 0
  fi
fi

mkdir -p "$transfer_root"
rm -f -- "$archive_staging" "$manifest_staging" "$archive" "$manifest"
trap 'rm -f -- "$archive_staging" "$manifest_staging"' EXIT

tar -cf "$archive_staging" -C "$results_root" "native-cache/$run_id"
digest="$(sha256_file "$archive_staging")"
mv "$archive_staging" "$archive"
printf '%s  %s\n' "$digest" "$run_id.tar" >"$manifest_staging"
mv "$manifest_staging" "$manifest"
trap - EXIT

echo "native-cache archive: finalized $run_id ($digest)" >&2
