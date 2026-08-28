#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$script_dir/../.." && pwd)"
output_file="$repository_root/docs/caching.md"

check=false
case "${1:-}" in
  "") ;;
  --check) check=true ;;
  *)
    echo "Usage: $0 [--check]" >&2
    exit 2
    ;;
esac
if (( $# > 1 )); then
  echo "Usage: $0 [--check]" >&2
  exit 2
fi

python_command=python3
if ! command -v "$python_command" >/dev/null 2>&1; then
  python_command=python
fi
if ! command -v "$python_command" >/dev/null 2>&1; then
  echo "generated documentation requires Python" >&2
  exit 127
fi

generated="$(
  PYTHONIOENCODING=utf-8 "$python_command" "$repository_root/scripts/ci/support-matrix.py" --markdown | sed 's/\r$//'
)"

if [[ "$check" == true ]]; then
  if [[ ! -f "$output_file" ]]; then
    echo "$output_file does not exist; run: just gen-docs" >&2
    exit 1
  fi
  if ! diff -q <(printf '%s\n' "$generated") "$output_file" >/dev/null; then
    echo "$output_file is out of date; run: just gen-docs" >&2
    diff -u "$output_file" <(printf '%s\n' "$generated") || true
    exit 1
  fi
  echo "generated documentation is up to date"
  exit 0
fi

printf '%s\n' "$generated" >"$output_file"
echo "generated: $output_file"
