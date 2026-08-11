#!/usr/bin/env bash
set -euo pipefail

# Runs on the remote qualification machine itself (invoked via
# `just bench-native-cache-remote`, which the local orchestrator in
# remote-native-cache.sh drives over SSH). Tags the run with the instance
# type and a fixed results directory, then dispatches to native-cache.sh.

usage() {
  echo "usage: $0 <smoke|run> <runs> <run-id>" >&2
  exit 2
}

mode="${1:-}"
runs="${2:-}"
run_id="${3:-}"

case "$mode" in
  smoke | run) ;;
  *) usage ;;
esac
[[ -n "$runs" && -n "$run_id" ]] || usage
[[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

export CARGO_RAIL_BENCH_INSTANCE_TYPE="${DEV_MACHINE_INSTANCE_TYPE:-unknown}"
export CARGO_RAIL_BENCH_RESULTS="$PWD/benchmark_results/native-cache/$run_id"

if [[ "$mode" == smoke ]]; then
  exec "$script_dir/native-cache.sh" smoke
fi
exec "$script_dir/native-cache.sh" run "$runs"
