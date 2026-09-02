#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKER="$SCRIPT_DIR/check-locked-cargo.sh"
TMP_ROOT=$(mktemp -d)
trap 'rm -rf -- "$TMP_ROOT"' EXIT

fail() {
  echo "locked Cargo inventory test failure: $*" >&2
  exit 1
}

mkdir -p "$TMP_ROOT/scripts/ci" "$TMP_ROOT/.zed" "$TMP_ROOT/docs" "$TMP_ROOT/examples" "$TMP_ROOT/src"
printf '%s\n' 'build:' '    cargo build --locked --workspace' >"$TMP_ROOT/justfile"
"$CHECKER" --root "$TMP_ROOT" || fail "locked command was rejected"

printf '%s\n' '#!/usr/bin/env bash' 'cargo test --workspace' >"$TMP_ROOT/scripts/ci/example.sh"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unlocked command was accepted"
fi

printf '%s\n' '#!/usr/bin/env bash' "cargo test \\" "  --locked \\" '  --workspace' >"$TMP_ROOT/scripts/ci/example.sh"
"$CHECKER" --root "$TMP_ROOT" || fail "multiline locked command was rejected"

printf '%s\n' '#!/usr/bin/env bash' '# cargo check --workspace' >"$TMP_ROOT/scripts/ci/example.sh"
"$CHECKER" --root "$TMP_ROOT" || fail "comment was treated as a command"

printf '%s\n' 'cargo test `' '  --workspace' >"$TMP_ROOT/scripts/ci/example.ps1"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unlocked PowerShell command was accepted"
fi

printf '%s\n' 'cargo test `' '  --locked `' '  --workspace' >"$TMP_ROOT/scripts/ci/example.ps1"
"$CHECKER" --root "$TMP_ROOT" || fail "multiline locked PowerShell command was rejected"

printf '%s\n' \
  '# cargo-rail: allow-unlocked-cargo: fixture resolution deliberately creates its lockfile.' \
  'cargo metadata --format-version=1' >"$TMP_ROOT/scripts/ci/example.sh"
"$CHECKER" --root "$TMP_ROOT" || fail "reasoned resolution boundary was rejected"

printf '%s\n' '[{"label":"check","command":"cargo","args":["check"]}]' >"$TMP_ROOT/.zed/tasks.json"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unlocked Zed Cargo task was accepted"
fi

printf '%s\n' '[{"label":"check","command":"just","args":["check"]}]' >"$TMP_ROOT/.zed/tasks.json"
"$CHECKER" --root "$TMP_ROOT" || fail "repository-front-door Zed task was rejected"

printf '%s\n' 'fn run() {' '  process::run("cargo", &["check"], None);' '}' >"$TMP_ROOT/src/example.rs"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unlocked Rust Cargo subprocess was accepted"
fi

printf '%s\n' 'fn run() {' '  process::run("cargo", &["check", "--locked"], None);' '}' >"$TMP_ROOT/src/example.rs"
"$CHECKER" --root "$TMP_ROOT" || fail "locked Rust Cargo subprocess was rejected"

printf '%s\n' 'Use cargo package to inspect the distributable source.' >"$TMP_ROOT/CONTRIBUTING.md"
"$CHECKER" --root "$TMP_ROOT" || fail "Markdown prose was treated as a command"

printf '%s\n' '```sh' 'cargo package' '```' >"$TMP_ROOT/examples/package.md"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unlocked Markdown Cargo command was accepted"
fi

printf '%s\n' '```console' '$ cargo package --locked' 'Packaging cargo-rail' '```' >"$TMP_ROOT/examples/package.md"
"$CHECKER" --root "$TMP_ROOT" || fail "locked Markdown Cargo command was rejected"

mkdir -p "$TMP_ROOT/scripts/ci/fixtures"
printf '%s\n' 'Name: GLIBC_2.35' >"$TMP_ROOT/scripts/ci/fixtures/readelf.txt"
"$CHECKER" --root "$TMP_ROOT" || fail "inert command fixture data was rejected"

: >"$TMP_ROOT/scripts/ci/unknown.rb"
if "$CHECKER" --root "$TMP_ROOT" >/dev/null 2>&1; then
  fail "unrecognized command-surface file type was accepted"
fi

echo "Locked Cargo inventory regression tests passed"
