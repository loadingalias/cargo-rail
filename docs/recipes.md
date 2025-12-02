# Recipes

Common workflows for cargo-rail.

---

## CI: Test affected crates

Tests only crates affected by the current changes.

### GitHub Actions

```yaml
name: CI
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0  # Required for change detection
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-rail
      - run: cargo rail test
      - run: cargo rail unify --check  # Fail if deps need unification
```

### GitLab CI

```yaml
variables:
  GIT_DEPTH: 0

test:
  script:
    - cargo install cargo-rail
    - cargo rail test
    - cargo rail unify --check
```

---

## CI: Parallel matrix testing

For larger workspaces, test affected crates in parallel.

```yaml
name: CI
on: [push, pull_request]

jobs:
  detect:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.affected.outputs.matrix }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-rail
      - id: affected
        run: cargo rail affected -f github-matrix -o $GITHUB_OUTPUT

  test:
    needs: detect
    if: needs.detect.outputs.matrix != '{"include":[]}'
    strategy:
      matrix: ${{ fromJson(needs.detect.outputs.matrix) }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test -p ${{ matrix.crate }}
```

---

## CI: Cache cargo-rail

```yaml
- uses: actions/cache@v4
  with:
    path: ~/.cargo/bin/cargo-rail
    key: cargo-rail-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
    restore-keys: cargo-rail-${{ runner.os }}-
- run: command -v cargo-rail || cargo install cargo-rail
```

---

## Unify dependencies

Consolidate workspace dependencies to `[workspace.dependencies]`.

```bash
# Preview changes
cargo rail unify --check

# Apply changes
cargo rail unify

# Apply with backup
cargo rail unify --backup

# Undo last unification
cargo rail unify undo

# List available backups
cargo rail unify undo --list
```

What it does:
- Moves shared deps to `[workspace.dependencies]`
- Updates members to use `workspace = true`
- Detects and removes unused deps
- Prunes dead features (empty no-ops)
- Computes MSRV from resolved graph
- Reports version conflicts and issues

---

## Pin transitive dependencies

Replaces workspace-hack crates (cargo-hakari). Detects transitive-only deps with fragmented features across targets and pins them.

```bash
cargo rail unify --pin-transitives
```

Or in `rail.toml`:

```toml
[unify]
pin_transitives = true
```

---

## Detect unused dependencies

Find deps declared in Cargo.toml but absent from the resolved graph.

```bash
# Detect only
cargo rail unify --check

# Auto-remove
cargo rail unify
```

In `rail.toml`:

```toml
[unify]
detect_unused = true   # Report unused deps
remove_unused = true   # Auto-remove (or false to just report)
```

---

## Handle major version conflicts

When the same dep has multiple major versions across members:

```toml
[unify]
major_version_conflict = "warn"  # Skip unification for that dep (safe)
# or
major_version_conflict = "bump"  # Force highest version (may require fixes)
```

---

## Split a crate

Extract a crate with full git history preserved.

```bash
# Configure the split
cargo rail split init my-crate

# Preview
cargo rail split run my-crate --check

# Execute
cargo rail split run my-crate
```

Preserves:
- Full commit history for the crate's files
- Deterministic commit SHAs (same input = same output)
- Commit mapping in git-notes for traceability

---

## Sync monorepo and split repos

After splitting, keep repos in sync.

```bash
# Bidirectional sync
cargo rail sync my-crate

# Push monorepo -> split repo
cargo rail sync my-crate --to-remote

# Pull split repo -> monorepo (creates PR branch)
cargo rail sync my-crate --from-remote
```

Changes from `--from-remote` go to a PR branch, never directly to main.

---

## Release workflow

```bash
# Preview release plan
cargo rail release run my-crate --check

# Release with patch bump
cargo rail release run my-crate --bump patch

# Release all crates (dependency order)
cargo rail release run --all --bump minor

# Skip crates.io publish (just tag)
cargo rail release run my-crate --skip-publish
```

---

## Validate before release

```bash
cargo rail check my-crate
cargo rail check --all
```

Validates:
- Clean working directory
- Valid Cargo.toml
- Changelog exists (if required)
- Dependencies are published

---

## Multi-target resolution

For cross-platform workspaces:

```toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
```

cargo-rail runs `cargo metadata --filter-platform` for each target. Features are computed as intersection across targets - only features needed everywhere go to workspace deps. Members add local features for their specific needs.

---

## Local development

```bash
# Generate config
cargo rail init

# Check if deps need unification (pre-commit hook)
cargo rail unify --check

# See affected crates
cargo rail affected

# Run tests for affected crates
cargo rail test

# Clean artifacts
cargo rail clean
```
