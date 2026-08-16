#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <storage-account> <cargo-rail-task9-run-id container> <run-id>" >&2
  exit 2
}

account="${1:-}"
container="${2:-}"
run_id="${3:-}"
[[ "$#" -eq 3 ]] || usage
[[ "$account" =~ ^[a-z0-9]{3,24}$ ]] || usage
[[ "$run_id" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || usage
expected_container="cargo-rail-task9-$run_id"
[[ "${#expected_container}" -le 63 && "$container" == "$expected_container" ]] || usage

command -v az >/dev/null || {
  echo "native-cache Azure cleanup requires az" >&2
  exit 2
}

container_exists() {
  az storage container exists \
    --account-name "$account" \
    --name "$container" \
    --auth-mode login \
    --only-show-errors \
    --query exists \
    --output tsv
}

exists="$(container_exists)"
case "$exists" in
  true)
    az storage container delete \
      --account-name "$account" \
      --name "$container" \
      --auth-mode login \
      --only-show-errors \
      --output none
    ;;
  false) ;;
  *)
    echo "native-cache Azure cleanup received an invalid existence result: $exists" >&2
    exit 2
    ;;
esac

exists="$(container_exists)"
[[ "$exists" == false ]] || {
  echo "native-cache Azure cleanup left container $account/$container" >&2
  exit 1
}

echo "native-cache Azure container absent: $account/$container"
