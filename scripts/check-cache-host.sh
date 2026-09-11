#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
# Fixtures own remote authority; machine enrollment must not supply cache hits.
unset CARGO_RAIL_CACHE_REMOTE CARGO_RAIL_CACHE_MODE CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT
export PYTHONDONTWRITEBYTECODE=1
if [[ $# != 0 ]]; then
  exec python3 scripts/check-cache-host.py "$@"
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
component_directory="$(cargo metadata --no-deps --format-version 1 --locked --offline |
    python3 -c 'import json, pathlib, sys; print(pathlib.Path(json.load(sys.stdin)["target_directory"]) / "cache-host")')"
scripts/check-compiler-fact-driver.sh --prepare "$component_directory"
# shellcheck source=/dev/null
source "$component_directory/compiler-driver-authority.env"
# Fixture Cargo commands own their own target directories.
unset CARGO_TARGET_DIR
python3 scripts/check-cache-host.py native "$(dirname "$component_directory")"
