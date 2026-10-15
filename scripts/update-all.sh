#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

[[ "$#" -eq 0 ]] || { echo 'usage: just update' >&2; exit 64; }
[[ "$(uname -s)" == Darwin ]] || { echo 'just update runs only on the local macOS workstation' >&2; exit 1; }
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
for command in uv cargo rustup gh cargo-audit cargo-deny rg; do
  command -v "$command" >/dev/null 2>&1 || { echo "missing update prerequisite: $command" >&2; exit 127; }
done

# uv runs the updater with the interpreter and pinned tooling group, isolated from Homebrew's Python.
export UV_PROJECT="$REPO_ROOT" UV_LOCKED=1
exec uv run --group tooling python "$SCRIPT_DIR/tooling/update.py"
