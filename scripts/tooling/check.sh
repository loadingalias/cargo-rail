#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/../.."
# uv runs every Python command with the interpreter and packages that the repository pins.
export UV_PROJECT="$PWD" UV_LOCKED=1
for command in uv shellcheck actionlint bash rg; do
  command -v "$command" >/dev/null || { echo "missing tooling check prerequisite: $command" >&2; exit 127; }
done
uv run python scripts/tooling/catalog.py validate
for script in scripts/check*.sh scripts/prepare-cranelift.sh scripts/release.sh scripts/tooling/*.sh scripts/update-all.sh; do bash -n "$script"; done
shellcheck -x scripts/check*.sh scripts/prepare-cranelift.sh scripts/release.sh scripts/tooling/*.sh scripts/update-all.sh
actionlint

# The tooling tests import the updater, which needs the tooling group's pinned libraries.
uv run --group tooling python -m unittest discover -s scripts/tooling -p 'test_*.py' -v
