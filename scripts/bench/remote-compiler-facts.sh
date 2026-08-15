#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
dev_machine="${DEV_MACHINE_BIN:-$HOME/dev-machines/dev-machine}"
operation="${1:-}"
target="${2:-}"

usage() {
  cat >&2 <<'USAGE'
usage:
  remote-compiler-facts.sh plan <target>
  remote-compiler-facts.sh smoke <target> --execute
  remote-compiler-facts.sh run <target> <runs> --execute

Targets: aws-linux-x64-bench, aws-linux-arm64-bench,
         aws-windows-x64-bench, azure-windows-arm64-profile
USAGE
  exit 2
}

case "$target" in
  aws-linux-x64-bench | aws-linux-arm64-bench)
    tool_profile=linux-qualification
    ;;
  aws-windows-x64-bench | azure-windows-arm64-profile)
    tool_profile=windows-qualification
    ;;
  *) usage ;;
esac
[[ -x "$dev_machine" ]] || {
  echo "dev-machine is not executable: $dev_machine" >&2
  exit 2
}

case "$operation" in
  plan)
    [[ "$#" -eq 2 ]] || usage
    "$dev_machine" resolve cargo-rail "$target"
    "$dev_machine" create cargo-rail "$target" --dry-run
    exit 0
    ;;
  smoke)
    [[ "$#" -eq 3 && "$3" == --execute ]] || usage
    runs=1
    ;;
  run)
    [[ "$#" -eq 4 && "$4" == --execute ]] || usage
    runs="$3"
    [[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 20 ]] || {
      echo "retained compiler-fact qualification requires at least 20 samples per lane" >&2
      exit 2
    }
    ;;
  *) usage ;;
esac

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$target-$operation"
destination="$repo_root/target/benchmarks/remote/compiler-facts/$target/$run_id"
orchestration_log="$destination.orchestration.log"
[[ ! -e "$destination" && ! -e "$orchestration_log" ]] || {
  echo "compiler-fact result or orchestration log already exists for $run_id" >&2
  exit 2
}
mkdir -p "$(dirname "$destination")"
exec > >(tee "$orchestration_log") 2>&1
echo "remote-compiler-facts: retaining orchestration log at $orchestration_log"

created=false
cleanup() {
  local status="$?"
  trap - EXIT INT TERM
  if [[ "$created" == true ]]; then
    echo "remote-compiler-facts: destroying $target and verifying attached storage deletion" >&2
    if ! "$dev_machine" kill cargo-rail "$target"; then
      echo "remote-compiler-facts: teardown failed for $target" >&2
      [[ "$status" -ne 0 ]] || status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

"$dev_machine" preflight cargo-rail "$target"
"$dev_machine" create cargo-rail "$target" --execute
created=true
"$dev_machine" bootstrap cargo-rail "$target" "$tool_profile"

set +e
"$dev_machine" just cargo-rail "$target" qualify-compiler-facts-remote \
  "$operation" "$runs" "$run_id"
qualification_status=$?
set -e

set +e
"$dev_machine" just cargo-rail "$target" bench-compiler-facts-archive "$run_id"
archive_status=$?
set -e
if [[ "$archive_status" -eq 0 ]]; then
  collection_status=1
  for attempt in 1 2 3; do
    set +e
    "$dev_machine" collect-results cargo-rail "$target" compiler-facts "$run_id" "$destination"
    collection_status=$?
    set -e
    [[ "$collection_status" -ne 0 ]] || break
    echo "remote-compiler-facts: collection attempt $attempt failed for $run_id" >&2
  done
  [[ "$collection_status" -eq 0 ]] || exit "$collection_status"
elif [[ "$qualification_status" -eq 0 ]]; then
  exit "$archive_status"
fi

if [[ "$qualification_status" -ne 0 ]]; then
  echo "remote-compiler-facts: qualification failed; available raw evidence was retained" >&2
  exit "$qualification_status"
fi
echo "remote-compiler-facts: collected $target results at $destination" >&2
