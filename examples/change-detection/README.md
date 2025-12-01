# Change Detection

Demonstrates `cargo rail affected` and `cargo rail test` for graph-aware change detection.

## What This Shows

1. **Graph-aware analysis** - Changes propagate through the dependency graph
2. **Smart categorization** - Infrastructure vs. docs-only vs. crate changes
3. **Customizable patterns** - Configure what triggers rebuilds
4. **CI integration** - Multiple output formats for GitHub Actions

## Demo

[change-detection demo](./demo.mp4)

## How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│  Git Changes                                                    │
│  ───────────                                                    │
│  .github/workflows/ci.yml  → infrastructure (all crates)       │
│  README.md                 → docs-only (no tests)              │
│  crates/core/src/lib.rs    → crate change → walks dep graph    │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Dependency Graph Walk                                          │
│  ────────────────────                                           │
│  core (changed)                                                 │
│    ├── api (depends on core)      → affected                   │
│    │     └── server (depends on api) → affected                │
│    └── cli (depends on core)      → affected                   │
│  utils (no changes)               → skipped                    │
└─────────────────────────────────────────────────────────────────┘
```

## Commands

```bash
# Show affected crates (auto-detects origin/main)
cargo rail affected

# Changes in last 5 commits
cargo rail affected --since HEAD~5

# Changes between two SHAs (CI mode)
cargo rail affected --from abc123 --to def456

# Output formats for CI
cargo rail affected -f github-matrix    # {"include":[{"crate":"core"},...]}
cargo rail affected -f names-only       # Just names, one per line
cargo rail affected -f json             # Full JSON output

# Run tests for only affected crates
cargo rail test

# Explain why each crate is being tested
cargo rail test --explain

# Pass arguments to test runner
cargo rail test -- --nocapture
```

## Configuration

```toml
# rail.toml

# Infrastructure files that trigger all-crates rebuild
[change-detection]
infrastructure = [
    ".github/**",
    "scripts/**",
    "justfile",
    "Makefile",
    "rust-toolchain.toml",
    "Cargo.lock",
]

# Custom categories for specialized workflows
[change-detection.custom]
# Verification models - separate from regular tests
verify = ["**/verify/**/*.rs"]
# Benchmark files - may want separate CI job
bench = ["**/benches/**"]
```

## Output Formats

| Format | Use Case | Example |
|--------|----------|---------|
| `text` (default) | Human-readable | Pretty-printed with colors |
| `json` | Programmatic access | Full structured output |
| `names-only` | Shell scripting | `core\napi\nserver` |
| `github-matrix` | GitHub Actions matrix | `{"include":[...]}` |
| `jsonl` | Streaming/logs | One JSON object per line |

## GitHub Actions Integration

```yaml
jobs:
  detect:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.affected.outputs.matrix }}
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - run: cargo install cargo-rail
      - id: affected
        run: |
          cargo rail affected \
            --from ${{ github.event.pull_request.base.sha }} \
            --to ${{ github.event.pull_request.head.sha }} \
            -f github-matrix \
            -o $GITHUB_OUTPUT

  test:
    needs: detect
    strategy:
      matrix: ${{ fromJson(needs.detect.outputs.matrix) }}
    steps:
      - run: cargo test -p ${{ matrix.crate }}
```

## Use Cases

- **CI optimization** - Only test what changed, not the entire workspace
- **Pre-commit hooks** - Quick validation of affected crates
- **PR reviews** - See exactly which crates a PR touches
- **Monorepo scale** - Essential for large workspaces (100+ crates)

Details in [`summary.toml`](./summary.toml).
