#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
dev_machine="${DEV_MACHINE_BIN:-$HOME/dev-machines/dev-machine}"
operation="${1:-}"
target="${2:-}"

usage() {
  cat >&2 <<'USAGE'
usage:
  remote-native-cache.sh plan <target>
  remote-native-cache.sh smoke <target> --execute
  remote-native-cache.sh run <target> <runs> --execute

Targets: aws-linux-x64-bench, aws-linux-arm64-bench,
         aws-windows-x64-bench, azure-windows-arm64-profile
USAGE
  exit 2
}

case "$target" in
  aws-linux-x64-bench | aws-linux-arm64-bench | aws-windows-x64-bench | azure-windows-arm64-profile)
    tool_profile=native-cache-qualification
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
    benchmark_mode=smoke
    ;;
  run)
    if [[ "$#" -eq 4 && "$4" == --execute ]]; then
      runs="$3"
    else
      usage
    fi
    [[ "$runs" =~ ^[0-9]+$ && "$runs" -ge 20 && "$runs" -le 30 ]] || {
      echo "retained native-cache qualification requires 20-30 rounds" >&2
      exit 2
    }
    benchmark_mode=run
    ;;
  *) usage ;;
esac

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$target-$benchmark_mode"
destination="$repo_root/target/benchmarks/remote/$target/$run_id"
orchestration_log="$destination.orchestration.log"
[[ ! -e "$destination" ]] || {
  echo "remote benchmark destination already exists: $destination" >&2
  exit 2
}
[[ ! -e "$orchestration_log" ]] || {
  echo "remote benchmark orchestration log already exists: $orchestration_log" >&2
  exit 2
}
mkdir -p "$(dirname "$destination")"
exec > >(tee "$orchestration_log") 2>&1
echo "remote-native-cache: retaining orchestration log at $orchestration_log"

created=false
cleanup() {
  local status="$?"
  trap - EXIT INT TERM
  if [[ "$created" == true ]]; then
    echo "remote-native-cache: destroying $target and verifying attached storage deletion" >&2
    if ! "$dev_machine" kill cargo-rail "$target"; then
      echo "remote-native-cache: teardown failed for $target" >&2
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
"$dev_machine" just cargo-rail "$target" bench-native-cache-remote \
  "$benchmark_mode" "$runs" "$run_id"
benchmark_status=$?
set -e

set +e
"$dev_machine" just cargo-rail "$target" bench-native-cache-archive "$run_id"
archive_status=$?
set -e
if [[ "$archive_status" -ne 0 ]]; then
  echo "remote-native-cache: could not archive exact run $run_id" >&2
  [[ "$benchmark_status" -ne 0 ]] && exit "$benchmark_status"
  exit "$archive_status"
fi

collection_status=1
for attempt in 1 2 3; do
  set +e
  "$dev_machine" collect-bench cargo-rail "$target" "$run_id" "$destination"
  collection_status=$?
  set -e
  [[ "$collection_status" -ne 0 ]] || break
  echo "remote-native-cache: collection attempt $attempt failed for exact run $run_id" >&2
done

if [[ "$collection_status" -ne 0 ]]; then
  echo "remote-native-cache: failed to collect exact benchmark run $run_id from $target" >&2
  exit "$collection_status"
fi
echo "remote-native-cache: collected $target results at $destination" >&2
if [[ "$benchmark_status" -ne 0 ]]; then
  echo "remote-native-cache: benchmark validation failed; raw results were retained" >&2
  exit "$benchmark_status"
fi
