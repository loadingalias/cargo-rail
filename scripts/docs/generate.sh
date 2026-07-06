#!/usr/bin/env bash
set -euo pipefail

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Documentation Generator
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#
# Generates docs/commands.md from the CLI's --help output.
# The CLI is the single source of truth.
#
# Usage:
#   ./scripts/docs/generate.sh           # Generate docs
#   ./scripts/docs/generate.sh --check   # Verify docs are up-to-date (CI mode)
#
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_FILE="$REPO_ROOT/docs/commands.md"
BINARY="$REPO_ROOT/target/release/cargo-rail"

CHECK_MODE=false

for arg in "$@"; do
  case $arg in
    --check)
      CHECK_MODE=true
      shift
      ;;
    *)
      echo "Unknown option: $arg"
      echo "Usage: $0 [--check]"
      exit 1
      ;;
  esac
done

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Build if needed
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

ensure_binary() {
  # Always build (cargo is incremental and will no-op when up to date).
  # This prevents regenerating docs from a stale binary.
  echo "Building cargo-rail (release)..."
  cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --quiet
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Generate markdown
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

generate_docs() {
  local output=""

  # Header
  output+="# Command Reference

> Auto-generated from \`cargo rail --help\`. Do not edit manually.
>
> Regenerate with: \`./scripts/docs/generate.sh\`

---

"

  # Main help
  output+="## cargo rail

\`\`\`
$("$BINARY" rail --help)
\`\`\`

---

"

  # Get list of subcommands (parse from help output)
  local commands
  commands=$("$BINARY" rail --help | awk '/^Commands:$/,/^Options:$/' | grep -E '^\s+\w' | awk '{print $1}' | grep -v '^help$')

  for cmd in $commands; do
    local help_output
    help_output=$("$BINARY" rail "$cmd" --help 2>&1 || true)

    output+="## cargo rail $cmd

\`\`\`
$help_output
\`\`\`

---

"

    # Check if this command has subcommands
    if echo "$help_output" | grep -q "^Commands:"; then
      local subcommands
      subcommands=$(echo "$help_output" | awk '/^Commands:$/,/^Options:$/' | grep -E '^\s+\w' | awk '{print $1}' | grep -v '^help$')

      for subcmd in $subcommands; do
        local subcmd_help
        subcmd_help=$("$BINARY" rail "$cmd" "$subcmd" --help 2>&1 || true)

        # Only add if help was successfully retrieved
        if [ -n "$subcmd_help" ] && ! echo "$subcmd_help" | grep -q "^error:"; then
          output+="### cargo rail $cmd $subcmd

\`\`\`
$subcmd_help
\`\`\`

---

"
        fi
      done
    fi
  done

  # Remove trailing ---
  output="${output%---

}"

  printf '%s\n' "$output" | sed 's/[[:blank:]]*$//'
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Main
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

main() {
  ensure_binary
  mkdir -p "$(dirname "$OUTPUT_FILE")"

  local generated
  generated=$(generate_docs)

  if [ "$CHECK_MODE" = true ]; then
    if [ ! -f "$OUTPUT_FILE" ]; then
      echo "ERROR: $OUTPUT_FILE does not exist"
      echo "Run: ./scripts/docs/generate.sh"
      exit 1
    fi

    if ! diff -q <(echo "$generated") "$OUTPUT_FILE" > /dev/null 2>&1; then
      echo "ERROR: docs/commands.md is out of date"
      echo ""
      echo "Diff:"
      diff <(echo "$generated") "$OUTPUT_FILE" || true
      echo ""
      echo "Run: ./scripts/docs/generate.sh"
      exit 1
    fi

    echo "docs/commands.md is up-to-date"
    exit 0
  fi

  echo "$generated" > "$OUTPUT_FILE"
  echo "Generated: $OUTPUT_FILE"
}

main
