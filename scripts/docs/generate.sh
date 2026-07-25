#!/usr/bin/env bash
set -euo pipefail

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Documentation Generator
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#
# Generates docs/commands.md from the CLI's --help output and
# docs/cache-capabilities.md from the native-cache production gates.
#
# Usage:
#   ./scripts/docs/generate.sh           # Generate docs
#   ./scripts/docs/generate.sh --check   # Verify docs are up-to-date (CI mode)
#
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_FILE="$REPO_ROOT/docs/commands.md"
CACHE_OUTPUT_FILE="$REPO_ROOT/docs/cache-capabilities.md"
BINARY="$REPO_ROOT/target/debug/cargo-rail"

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
  echo "Building cargo-rail..."
  cargo build --locked --manifest-path "$REPO_ROOT/Cargo.toml" --quiet
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

generate_cache_capabilities() {
  local native_cache="$REPO_ROOT/src/compiler/native_cache.rs"
  local run_command="$REPO_ROOT/src/commands/run.rs"
  local rustc_release cargo_release
  rustc_release=$(sed -n 's/^const GRADUATED_RUSTC_RELEASE: &str = "\(.*\)";/\1/p' "$native_cache")
  cargo_release=$(sed -n 's/^const GRADUATED_CARGO_RELEASE: &str = "\(.*\)";/\1/p' "$native_cache")

  [[ -n "$rustc_release" && -n "$cargo_release" ]] || {
    echo "ERROR: could not read graduated native-cache toolchain releases" >&2
    exit 1
  }
  grep -Fq '("unix-macos-aarch64", "aarch64-apple-darwin")' "$native_cache"
  grep -Fq '("unix-linux-aarch64", "aarch64-unknown-linux-gnu")' "$native_cache"
  grep -Fq 'ActionKind::Build | ActionKind::Distribution' "$run_command"
  grep -Fq 'std::env::var_os("CARGO_INCREMENTAL")' "$run_command"

  cat <<EOF
# Cache Capability Matrix

> Auto-generated from the native-cache production gates. Do not edit manually.
>
> Regenerate with: \`./scripts/docs/generate.sh\`

## Cache layers

| Layer | Current support | Authority boundary |
|---|---|---|
| Compiler-evidence cache | Workspace-only \`unify\` observations with complete revalidation | Diagnostic evidence; never restores Cargo artifacts |
| Hermetic whole-action cache | Current-host macOS pure-Rust \`cargo check\` class | Verified action/result manifest and isolated output tree |
| Native compiler-result cache | Eligible library metadata/rlib invocations listed below | Verified per-invocation action/result binding through Cargo's wrapper boundary |

## Native hosts and toolchains

| Host | Cargo | rustc | Status |
|---|---:|---:|---|
| \`aarch64-apple-darwin\` | \`$cargo_release\` | \`$rustc_release\` | Shipped |
| \`aarch64-unknown-linux-gnu\` | \`$cargo_release\` | \`$rustc_release\` | Shipped |
| Every other host or toolchain release | — | — | Deliberately bypassed |

## Native compilation classes

| Class | Status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Shipped | One declared crate root, complete observed Rust inputs, dep-info, \`.rmeta\`, optional \`.rlib\`, Rust-only dependency artifacts, no linker responsibility |
| Incremental compilation | Deliberately bypassed | Requires \`CARGO_INCREMENTAL=0\`; forced incremental compilation also bypasses |
| Binary, test, example, and benchmark linking | Deliberately bypassed | Linker-producing invocations are not graduated |
| \`dylib\`, \`cdylib\`, and \`staticlib\` | Deliberately bypassed | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Deliberately bypassed | Compile-time filesystem/process reads are not completely observed |
| Build scripts and generated output | Deliberately bypassed | Normal Cargo messages do not prove the ordered instruction stream, runtime reads, generated tree, or freshness |
| Native dependencies and \`links\` contracts | Deliberately bypassed | External compiler, archiver, linker, SDK, and discovery inputs are incomplete |
| rustdoc and doctests | Deliberately bypassed | Stable Cargo output does not enumerate the complete documentation tree; doctest execution is separate |
| Cross compilation and custom target specifications | Deliberately bypassed | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom compiler wrappers | Preserved; cargo-rail bypasses | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI \`--config\` and action-defined environments | Deliberately bypassed | Effective build configuration or environment is outside the graduated direct-action contract |

See [Caching](caching.md) for activation, telemetry, benchmark evidence, and the graduation rules behind this matrix.
EOF
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Main
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

main() {
  ensure_binary
  mkdir -p "$(dirname "$OUTPUT_FILE")"

  local generated
  generated=$(generate_docs)
  local generated_cache_capabilities
  generated_cache_capabilities=$(generate_cache_capabilities)

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

    if [ ! -f "$CACHE_OUTPUT_FILE" ]; then
      echo "ERROR: $CACHE_OUTPUT_FILE does not exist"
      echo "Run: ./scripts/docs/generate.sh"
      exit 1
    fi

    if ! diff -q <(echo "$generated_cache_capabilities") "$CACHE_OUTPUT_FILE" > /dev/null 2>&1; then
      echo "ERROR: docs/cache-capabilities.md is out of date"
      echo ""
      echo "Diff:"
      diff <(echo "$generated_cache_capabilities") "$CACHE_OUTPUT_FILE" || true
      echo ""
      echo "Run: ./scripts/docs/generate.sh"
      exit 1
    fi

    echo "generated documentation is up-to-date"
    exit 0
  fi

  echo "$generated" > "$OUTPUT_FILE"
  echo "$generated_cache_capabilities" > "$CACHE_OUTPUT_FILE"
  echo "Generated: $OUTPUT_FILE"
  echo "Generated: $CACHE_OUTPUT_FILE"
}

main
