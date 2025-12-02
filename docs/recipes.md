# Recipes

Common workflows for cargo-rail.

---

## CI: Test only affected crates

The simplest setup. Tests only crates affected by the current changes.

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

For larger workspaces, test affected crates in parallel using a matrix.

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

Avoid reinstalling on every run:

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

# Apply with backup (can undo later)
cargo rail unify --backup

# Undo last unification
cargo rail unify undo

# List available backups
cargo rail unify undo --list
```

---

## Pin transitive dependencies (hakari replacement)

cargo-rail can pin transitive-only dependencies to prevent feature fragmentation,
replacing the need for workspace-hack crates.

```bash
cargo rail unify --pin-transitives
```

Or in `rail.toml`:

```toml
[unify]
pin_transitives = true
```

---

## Detect and remove unused dependencies

Find deps declared in Cargo.toml but not in the resolved dependency graph:

```bash
# Detect only (reports in output)
cargo rail unify --check  # with detect_unused = true in config

# Auto-remove
cargo rail unify  # with remove_unused = true in config
```

In `rail.toml`:

```toml
[unify]
detect_unused = true
remove_unused = true  # auto-remove, or just detect without removing
```

---

## Split a crate to its own repo

Extract a crate with full git history preserved:

```bash
# Configure the split
cargo rail split init my-crate

# Preview what would happen
cargo rail split run my-crate --check

# Execute the split
cargo rail split run my-crate
```

The split preserves:
- Full commit history for the crate's files
- Deterministic commit SHAs (same input = same output)
- Commit mapping stored in git-notes for traceability

---

## Sync between monorepo and split repos

After splitting, keep repos in sync:

```bash
# Sync changes (bidirectional)
cargo rail sync my-crate

# Push monorepo changes to split repo
cargo rail sync my-crate --to-remote

# Pull split repo changes to monorepo (creates PR branch)
cargo rail sync my-crate --from-remote
```

Safety: Changes from `--from-remote` go to a PR branch, never directly to main.

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

Check that crates are ready for release:

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

For cross-platform workspaces, cargo-rail resolves dependencies across all targets:

```toml
# rail.toml
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
```

This ensures unified dependencies work on all platforms, computing the intersection
of required features rather than the union.

---

## Local development

```bash
# Generate/update config
cargo rail init

# Check if deps need unification (useful as pre-commit hook)
cargo rail unify --check

# See what crates are affected by your changes
cargo rail affected

# Run tests for affected crates only
cargo rail test

# Clean up generated artifacts
cargo rail clean
```
