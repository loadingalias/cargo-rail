# Change Detection: Standalone

For projects **without** a dedicated task runner (no just, make, or xtask).

## Strategy

`cargo rail run` handles both change detection and execution using built-in surfaces.

## Usage

### Local Development

```bash
# See what changed and why
cargo rail plan --merge-base --explain

# Run tests for affected crates
cargo rail run --merge-base --surface test

# Preview commands without running
cargo rail run --merge-base --dry-run --print-cmd

# Use a named profile
cargo rail run --workflow ci

# Force full workspace test
cargo rail run --all --surface test
```

### GitHub Actions

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      build: ${{ steps.rail.outputs.build }}
      test: ${{ steps.rail.outputs.test }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: loadingalias/cargo-rail-action@v3
        id: rail
        with:
          mode: full
          args: '--explain'

  test:
    needs: plan
    if: needs.plan.outputs.test == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo rail run --merge-base --surface test

  build:
    needs: plan
    if: needs.plan.outputs.build == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo rail run --merge-base --surface build
```

## Built-in Surfaces

| Surface | Command |
|---------|---------|
| `build` | `cargo check --workspace` |
| `test` | `cargo test` or `cargo nextest run` |
| `bench` | `cargo bench --workspace` |
| `docs` | `cargo doc --workspace --no-deps` |
| `infra` | No executor (CI gating only) |

## Profiles

Profiles bundle surfaces together:

```toml
[run.profile.ci]
surfaces = ["build", "test"]

[run.profile.nightly]
surfaces = ["build", "test", "docs", "bench"]
```

Run with: `cargo rail run --profile ci`

## Workflow Mapping

Map CI workflow names to profiles:

```toml
[run.workflow]
commit = "ci"
nightly = "nightly"
```

Run with: `cargo rail run --workflow commit`

## Real Examples

- [tokio](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio) — no task runner
- [helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db) — no task runner
