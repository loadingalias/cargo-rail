#!/usr/bin/env bash
set -euo pipefail

ROOT=""
if [[ ${1:-} == --root ]]; then
  ROOT=${2:?missing path after --root}
  shift 2
fi
if [[ $# -ne 0 ]]; then
  echo "usage: check-locked-cargo.sh [--root PATH]" >&2
  exit 2
fi

if [[ -z "$ROOT" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
fi

status=0
check_statement() {
  local file=$1
  local line_number=$2
  local statement=$3
  local allow_unlocked=$4
  local trimmed=${statement#"${statement%%[![:space:]]*}"}

  case "$trimmed" in
    echo\ * | printf\ * | step\ *) return ;;
  esac
  if [[ "$allow_unlocked" == true ]]; then
    return
  fi

  if [[ "$statement" =~ (^|[[:space:]\`\"\'])cargo[[:space:]]+(build|check|clippy|test|bench|rustc|run|doc|miri|metadata|publish)($|[[:space:]\`\"\']) ]] \
    && [[ "$statement" != *"--locked"* ]]; then
    echo "$file:$line_number: routine Cargo command must use --locked" >&2
    echo "  $statement" >&2
    status=1
  fi

  if [[ "$statement" =~ (^|[[:space:]\`\"\'])cargo[[:space:]]+nextest[[:space:]]+run($|[[:space:]\`\"\']) ]] \
    && [[ "$statement" != *"--locked"* ]]; then
    echo "$file:$line_number: cargo-nextest execution must use --locked" >&2
    echo "  $statement" >&2
    status=1
  fi

  if [[ "$statement" =~ (^|[[:space:]\`\"\'])cargo[[:space:]]+llvm-cov[[:space:]]+(nextest|test)($|[[:space:]\`\"\']) ]] \
    && [[ "$statement" != *"--locked"* ]]; then
    echo "$file:$line_number: cargo-llvm-cov execution must use --locked" >&2
    echo "  $statement" >&2
    status=1
  fi
}

scan_text_file() {
  local path=$1
  local relative=$2
  local statement=""
  local statement_line=0
  local line_number=0
  local allow_unlocked=false
  while IFS= read -r line || [[ -n "$line" ]]; do
    line_number=$((line_number + 1))
    if [[ -z "$statement" ]]; then
      if [[ "$line" =~ ^[[:space:]]*(#|//) ]]; then
        if [[ "$line" =~ cargo-rail:[[:space:]]+allow-unlocked-cargo:[[:space:]]+.+ ]]; then
          allow_unlocked=true
        fi
        continue
      fi
      statement_line=$line_number
    fi
    statement+=" ${line%\\}"
    if [[ "$line" == *\\ ]]; then
      continue
    fi
    check_statement "$relative" "$statement_line" "$statement" "$allow_unlocked"
    statement=""
    allow_unlocked=false
  done <"$path"
  if [[ -n "$statement" ]]; then
    check_statement "$relative" "$statement_line" "$statement" "$allow_unlocked"
  fi
}

scan_zed_tasks() {
  local path=$1
  local relative=$2
  if ! jq -e 'type == "array"' "$path" >/dev/null; then
    echo "$relative: Zed task authority must be a JSON array" >&2
    status=1
    return
  fi
  while IFS= read -r task; do
    command=$(jq -r '.command' <<<"$task")
    args=$(jq -r '(.args // []) | join(" ")' <<<"$task")
    check_statement "$relative" 1 "$command $args" false
  done < <(jq -c '.[] | select((.command // "") == "cargo" or ((.command // "") | startswith("cargo ")))' "$path")
}

if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  inventory_command=(git -C "$ROOT" ls-files --cached --others --exclude-standard -- justfile README.md scripts .github .zed)
else
  inventory_command=(find "$ROOT" -type f)
fi

while IFS= read -r listed; do
  [[ -n "$listed" ]] || continue
  if [[ "$listed" == "$ROOT"/* ]]; then
    relative=${listed#"$ROOT"/}
    path=$listed
  else
    relative=$listed
    path="$ROOT/$listed"
  fi
  case "$relative" in
    justfile | README.md) ;;
    scripts/*.sh | scripts/**/*.sh | scripts/*.py | scripts/**/*.py | scripts/*.json | scripts/**/*.json)
      [[ "$relative" == *-test.sh ]] && continue
      ;;
    .github/*.yaml | .github/**/*.yaml | .github/*.yml | .github/**/*.yml) ;;
    .zed/tasks.json)
      scan_zed_tasks "$path" "$relative"
      continue
      ;;
    .zed/*.json) ;;
    scripts/* | .github/* | .zed/*)
      echo "$relative: unrecognized command-surface file type" >&2
      status=1
      continue
      ;;
    *) continue ;;
  esac
  scan_text_file "$path" "$relative"
done < <("${inventory_command[@]}" | LC_ALL=C sort -u)

if [[ -d "$ROOT/src" ]]; then
  if ! python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
patterns = (
    re.compile(r'process::run\(\s*"cargo"\s*,\s*&\[(.*?)\]\s*,', re.DOTALL),
    re.compile(r'Command::new\(\s*"cargo"\s*\)(.*?);', re.DOTALL),
)
routine = re.compile(r'"(?:build|check|clippy|test|bench|rustc|run|doc|miri|metadata|publish|nextest)"')
failed = False
for path in sorted((root / "src").rglob("*.rs")):
    source = path.read_text(encoding="utf-8")
    for pattern in patterns:
        for match in pattern.finditer(source):
            invocation = match.group(0)
            if routine.search(invocation) and '"--locked"' not in invocation:
                line = source.count("\n", 0, match.start()) + 1
                print(
                    f"{path.relative_to(root)}:{line}: Rust Cargo subprocess must use --locked",
                    file=sys.stderr,
                )
                failed = True
sys.exit(1 if failed else 0)
PY
  then
    status=1
  fi
fi

exit "$status"
