#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-2}"
exec just test ci
