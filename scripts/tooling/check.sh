#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/../.."
for command in python3 shellcheck actionlint bash rg; do
  command -v "$command" >/dev/null || { echo "missing tooling check prerequisite: $command" >&2; exit 127; }
done
python3 scripts/tooling/catalog.py validate
for script in scripts/check*.sh scripts/prepare-cranelift.sh scripts/tooling/*.sh scripts/update-all.sh; do bash -n "$script"; done
shellcheck -x scripts/check*.sh scripts/prepare-cranelift.sh scripts/tooling/*.sh scripts/update-all.sh
actionlint

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
python3 -m venv "$temporary/python"
python="$temporary/python/bin/python"
requirements=()
while IFS= read -r requirement; do requirements+=("$requirement"); done < <(
  python3 - <<'PY'
import sys
sys.path.insert(0,'scripts/tooling')
from catalog import read
for name,version in read()['updater'].items(): print(f'{name}=={version}')
PY
)
"$python" -m pip install --disable-pip-version-check --quiet "${requirements[@]}"
"$python" -m unittest discover -s scripts/tooling -p 'test_*.py' -v
