# How to Use cargo-rail Change Detection

This guide covers everything you need to configure intelligent change detection for local development and CI/CD.

## Core Concept

cargo-rail analyzes git changes and classifies them into **surfaces**:

| Surface | Triggered By | What It Means |
|---------|--------------|---------------|
| `build` | `.rs` files, `Cargo.toml` | Compile-time changes |
| `test` | Source files, test fixtures | Test execution needed |
| `bench` | `benches/`, perf-related | Benchmarks affected |
| `docs` | `.md`, doc comments | Documentation changes |
| `infra` | CI, scripts, tooling | Infrastructure changes |

The planner outputs which surfaces are active and which crates are affected. You decide what commands to run.

---

## Quick Start

### Local Development

```bash
# See what changed and why
cargo rail plan --merge-base --explain

# Run tests for affected crates only
cargo rail run --merge-base --surface test

# Full workspace (skip change detection)
cargo rail run --all --surface test
```

### CI Integration

```yaml
- uses: loadingalias/cargo-rail-action@v3
  id: plan
  with:
    since: ${{ github.event.pull_request.base.sha || github.event.before }}

- name: Test
  if: steps.plan.outputs.test == 'true'
  env:
    SCOPE_JSON: ${{ steps.plan.outputs.scope-json }}
  run: |
    SCOPE_MODE=$(echo "$SCOPE_JSON" | jq -r '.mode')
    if [ "$SCOPE_MODE" = "workspace" ]; then
      cargo nextest run --workspace
    elif [ "$SCOPE_MODE" = "crates" ]; then
      mapfile -t CRATES < <(echo "$SCOPE_JSON" | jq -r '.crates[]')
      ARGS=()
      for crate in "${CRATES[@]}"; do
        ARGS+=(-p "$crate")
      done
      cargo nextest run "${ARGS[@]}"
    else
      echo "planner enabled test surface, but no package-scoped work was selected"
    fi
```

Use `scope-json` when a downstream job or script needs the exact execution handoff. Use `impact` only for explanation and debugging. Use action `mode: debug` only when you need `plan-json` for deeper debugging.

---

## Configuration

### rail.toml

```toml
[change-detection]
# Files that trigger full rebuild
infrastructure = [
  ".github/**",
  "scripts/**",
  "justfile",
  "Cargo.lock",
  "rust-toolchain.toml",
]

# Confidence profile: strict, balanced, fast
confidence_profile = "balanced"

# Optional: stricter for bot PRs (dependabot, renovate)
bot_pr_confidence_profile = "strict"

# Custom surface patterns (appear as plan outputs, not profile inputs)
[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
e2e = ["tests/e2e/**"]
```

**Note:** Custom surfaces like `custom:benchmarks` are plan **outputs** for CI gating. They cannot be used in `[run.profile.X].surfaces`. In CI, read them from `scope-json.surfaces["custom:<name>"]` or use the action's `custom_<name>` outputs.

### Custom Profiles

Define your own execution profiles:

```toml
[run]
default_profile = "local"

[run.profile.quick]
surfaces = ["build"]

[run.profile.full]
surfaces = ["build", "test", "docs", "bench"]

[run.profile.release-check]
surfaces = ["build", "test"]
run_args = ["--", "--release"]

# Map workflow names to profiles
[run.workflow]
commit = "ci"
nightly = "full"
```

Usage:

```bash
cargo rail run --profile quick
cargo rail run --workflow commit
```

---

## Local Scripts

### scripts/test/test.sh

```bash
#!/usr/bin/env bash
set -euo pipefail

ARG="${1:-}"

# Specific crate: direct test
if [ -n "$ARG" ] && [ "$ARG" != "--all" ]; then
  cargo nextest run -p "$ARG" --all-features
  exit 0
fi

# Full workspace
if [ "$ARG" = "--all" ]; then
  cargo nextest run --workspace --all-features
  exit 0
fi

# Smart: affected crates only
if [ -n "${RAIL_SINCE:-}" ]; then
  cargo rail run --since "$RAIL_SINCE" --surface test
else
  cargo rail run --merge-base --surface test
fi
```

### scripts/check/check.sh

```bash
#!/usr/bin/env bash
set -euo pipefail

# Always run full workspace for these (fast, essential)
cargo fmt --all -- --check
cargo deny check all
cargo audit

# Smart checks for expensive operations
if [ "${1:-}" = "--all" ]; then
  cargo clippy --workspace --all-targets -- -D warnings
  cargo check --workspace --all-targets
else
  cargo rail run --merge-base --surface build
fi
```

### justfile

```make
# Smart commands (change detection)
check:
    @scripts/check/check.sh

test crate="":
    @scripts/test/test.sh "{{crate}}"

build:
    cargo rail run --merge-base --surface build

# Full workspace commands
check-all:
    @scripts/check/check.sh --all

test-all:
    @scripts/test/test.sh --all

# Explainability
why:
    cargo rail plan --merge-base --explain

dry-run:
    cargo rail run --merge-base --dry-run --print-cmd
```

---

## GitHub Actions

### Commit Workflow

```yaml
name: CI

on:
  push:
    branches: ["**"]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always

jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      build: ${{ steps.plan.outputs.build }}
      test: ${{ steps.plan.outputs.test }}
      docs: ${{ steps.plan.outputs.docs }}
      infra: ${{ steps.plan.outputs.infra }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: loadingalias/cargo-rail-action@v3
        id: plan
        with:
          since: ${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha || github.event.before }}

  ci:
    needs: plan
    if: needs.plan.outputs.build == 'true' || needs.plan.outputs.test == 'true' || needs.plan.outputs.infra == 'true'
    runs-on: ubuntu-latest
    env:
      RAIL_SINCE: ${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha || github.event.before }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Setup
        run: |
          rustup update stable
          cargo install cargo-nextest

      - name: Check
        run: just ci-check

      - name: Test
        run: just test-all

      - name: Upload Decision Receipt
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: cargo-rail-receipt
          path: target/cargo-rail/receipts/*.json
          if-no-files-found: ignore
```

### Showing What Changed in GitHub UI

The cargo-rail-action automatically renders a step summary. For custom visibility:

```yaml
- name: Build Plan
  id: plan
  run: |
    cargo rail plan --merge-base -f github >> "$GITHUB_OUTPUT"

    # Write human-readable summary to GitHub UI
    echo "## Change Detection Summary" >> "$GITHUB_STEP_SUMMARY"
    echo "" >> "$GITHUB_STEP_SUMMARY"
    cargo rail plan --merge-base --explain >> "$GITHUB_STEP_SUMMARY"

- name: Show What Will Run
  run: |
    echo "### Execution Plan" >> "$GITHUB_STEP_SUMMARY"
    echo "\`\`\`" >> "$GITHUB_STEP_SUMMARY"
    cargo rail run --merge-base --dry-run --print-cmd >> "$GITHUB_STEP_SUMMARY"
    echo "\`\`\`" >> "$GITHUB_STEP_SUMMARY"
```

### Scheduled Workflows (Benchmarks, Nightly)

For scheduled runs, you need a baseline. Options:

**Option 1: Run everything (simplest)**

```yaml
on:
  schedule:
    - cron: '0 2 * * *'

jobs:
  bench:
    steps:
      - uses: actions/checkout@v4
      - run: cargo bench --workspace
```

**Option 2: Compare against last release**

```yaml
- name: Benchmark since last release
  run: |
    LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "HEAD~50")
    cargo rail run --since "$LAST_TAG" --surface bench
```

**Option 3: Track last successful run**

```yaml
- name: Get baseline
  id: baseline
  uses: actions/cache@v4
  with:
    path: .last-bench-sha
    key: bench-baseline-${{ github.ref }}

- name: Run benchmarks
  run: |
    BASELINE=$(cat .last-bench-sha 2>/dev/null || echo "HEAD~50")
    cargo rail run --since "$BASELINE" --surface bench
    echo "${{ github.sha }}" > .last-bench-sha
```

---

## Using the Planner as an Oracle

The most flexible pattern: use planner gates for surface decisions and the scope contract for package selection. Do not reconstruct execution scope from `impact`.

```yaml
- id: plan
  uses: loadingalias/cargo-rail-action@v3

- name: Custom Test Suite
  if: steps.plan.outputs.test == 'true'
  run: |
    SCOPE='${{ steps.plan.outputs.scope-json }}'
    MODE="$(echo "$SCOPE" | jq -r '.mode')"

    if [ "$MODE" = "workspace" ]; then
      cargo nextest run --workspace -P ci
    elif [ "$MODE" = "crates" ]; then
      mapfile -t CRATES < <(echo "$SCOPE" | jq -r '.crates[]')
      ARGS=()
      for crate in "${CRATES[@]}"; do
        ARGS+=(-p "$crate")
      done
      cargo nextest run -P ci "${ARGS[@]}"
    else
      echo "no package-scoped work selected"
    fi
```

Access planner data in scripts:

```bash
# Get the compact execution handoff
SCOPE=$(cargo rail plan --merge-base -f json | jq -c '.scope')

# Check if the planner selected targeted crates
if echo "$SCOPE" | jq -e '.mode == "crates"' > /dev/null; then
  mapfile -t CRATES < <(echo "$SCOPE" | jq -r '.crates[]')
  ARGS=()
  for crate in "${CRATES[@]}"; do
    ARGS+=(-p "$crate")
  done
  cargo nextest run "${ARGS[@]}"
fi
```

---

## Decision Receipts

Every `cargo rail run` writes a receipt to `target/cargo-rail/receipts/`. Upload these in CI for debugging:

```yaml
- uses: actions/upload-artifact@v4
  if: always()
  with:
    name: cargo-rail-receipts
    path: target/cargo-rail/receipts/*.json
```

The receipt contains:
- Exact refs compared
- All files analyzed with classifications
- Surface decisions with reasoning
- Commands executed

---

## Confidence Profiles

| Profile | Behavior | Use Case |
|---------|----------|----------|
| `strict` | Conservative, expands to transitive deps | Release branches, bot PRs |
| `balanced` | Default behavior | Normal development |
| `fast` | Minimal expansion | Large repos, quick iteration |

```toml
[change-detection]
confidence_profile = "balanced"
bot_pr_confidence_profile = "strict"
```

Override at runtime:

```bash
cargo rail plan --confidence-profile strict
```

---

## Troubleshooting

### Why did this run/skip?

```bash
# Human-readable explanation
cargo rail plan --merge-base --explain

# Full trace with rule IDs
cargo rail plan --merge-base -f json | jq '.trace'
```

### Force full run

```bash
cargo rail run --all --surface test
```

### Compare two plans

```bash
cargo rail plan --since HEAD~5 -f json > plan-a.json
cargo rail plan --since HEAD~10 -f json > plan-b.json
cargo rail diff-hash plan-a.json plan-b.json
```

---

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success (or no-op if nothing to do) |
| 1 | Check mode found changes (expected for `--check`) |
| 2 | Error |

---

## Summary

1. **Configure** `rail.toml` with your infrastructure patterns and custom profiles
2. **Use scripts** that work both locally and in CI via environment variables
3. **Gate CI jobs** on planner outputs (`build`, `test`, `docs`, `infra`)
4. **Show decisions** in GitHub step summaries for visibility
5. **Upload receipts** for debugging unexpected behavior
6. **Use `--all`** when you need to bypass change detection
