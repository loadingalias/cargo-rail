#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

[[ "$#" -eq 0 ]] || { echo 'usage: just update' >&2; exit 64; }
[[ "$(uname -s)" == Darwin ]] || { echo 'just update runs only on the local macOS workstation' >&2; exit 1; }
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
for command in python3 cargo rustup gh cargo-audit cargo-deny rg; do
  command -v "$command" >/dev/null 2>&1 || { echo "missing update prerequisite: $command" >&2; exit 127; }
done

temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-update.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
# Isolate updater libraries from the user's Homebrew/Python installation.
python3 -m venv "$temporary/python"
python="$temporary/python/bin/python"
requirements=()
while IFS= read -r requirement; do requirements+=("$requirement"); done < <(
  python3 - "$REPO_ROOT/.config/tooling.toml" <<'PY'
import sys,tomllib
with open(sys.argv[1],'rb') as stream: config=tomllib.load(stream)
for name,version in config['updater'].items(): print(f'{name}=={version}')
PY
)
"$python" -m pip install --disable-pip-version-check --quiet "${requirements[@]}"

"$python" "$SCRIPT_DIR/tooling/update.py"
