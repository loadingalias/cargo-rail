<p align="center">
  <img src="https://socialify.git.ci/loadingalias/cargo-rail/image?font=Jost&language=1&name=1&owner=1&pattern=Solid&theme=Auto" alt="cargo-rail" width="640" height="320" />
</p>

<p align="center">
  <a href="https://crates.io/crates/cargo-rail"><img src="https://img.shields.io/crates/v/cargo-rail.svg" alt="Crates.io"></a>
  <a href="https://crates.io/crates/cargo-rail"><img src="https://img.shields.io/crates/d/cargo-rail.svg" alt="Downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/crates/msrv/cargo-rail" alt="MSRV"></a>
</p>

<p align="center">
  <a href="#install">Install</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#commands">Commands</a> •
  <a href="#real-world-results">Results</a> •
  <a href="https://github.com/loadingalias/cargo-rail-action">GitHub Action</a>
</p>

---

## What It Replaces

| Problem | Before | After |
|---------|--------|-------|
| **Build graph drift** | `cargo-hakari`, workspace-hack crates | `cargo rail unify` |
| **Unused deps** | `cargo-udeps`, `cargo-machete`, `cargo-shear` | `cargo rail unify` |
| **Dead features** | `cargo-features-manager`, manual audit | `cargo rail unify` |
| **MSRV computation** | `cargo-msrv`, compile-and-fail loops | `cargo rail unify` |
| **CI waste** | `paths-filter` + shell scripts | `cargo rail affected` |
| **CI costs** | Test everything, bill for everything | Test what changed |
| **Crate extraction** | `git subtree`, `git-filter-repo`, Google's Copybara | `cargo rail split` |
| **Release orchestration** | `release-plz`, `cargo-release`, `git-cliff` | `cargo rail release` |

**11 dependencies. One config file.**

---

## Install

```bash
cargo install cargo-rail
```

Optionally, install via the [pre-built binaries](https://github.com/loadingalias/cargo-rail/releases) or `cargo binstall cargo-rail`

---

## MSRV policy

- **MSRV source of truth**: `Cargo.toml` (`rust-version`, written as `major.minor.patch`)
- **Cargo requirement**: Cargo shipped with that Rust release (newer Cargo is fine)
- **CI**: builds on MSRV to prevent accidental bumps
- **Workspaces**: `cargo rail unify` writes `[workspace.package].rust-version`; enable `[unify].enforce_msrv_inheritance = true` to set `[package].rust-version = { workspace = true }` in member crates

---

## Quick Start

```bash
cargo rail init              # generate .config/rail.toml
cargo rail unify --check     # preview what would change (read-only)
cargo rail unify             # apply changes
```

[Demo](https://github.com/user-attachments/assets/93f34633-aa0e-4cde-8723-c81f3f474bac) using `ripgrep` codebase.

<sub>*`cargo rail unify` on ripgrep — 9 deps unified, 6 dead features pruned*</sub>

---

## Commands

### `affected` / `test`

Graph-aware change detection. Only test what's affected:

```bash
cargo rail affected                    # list affected crates
cargo rail affected --merge-base       # compare against merge-base (CI)
cargo rail affected -f cargo-args      # output: -p crate1 -p crate2
cargo rail affected -f github-matrix   # output: JSON matrix for Actions
cargo rail test                        # run tests for affected crates
cargo rail test --explain              # show why each crate is affected
```

**CI Integration:**

```yaml
- uses: loadingalias/cargo-rail-action@v1
  id: rail

- run: cargo nextest run ${{ steps.rail.outputs.cargo-args }}
  if: steps.rail.outputs.should-test == 'true'
```

### `unify`

Dependency unification based on Cargo's resolved output:

```bash
cargo rail unify --check    # preview changes (exits 1 if drift detected)
cargo rail unify            # apply to workspace
cargo rail unify --explain  # show reasoning for each change
cargo rail unify undo       # restore from backup
```

What it does:

- **Unifies versions** — writes to `[workspace.dependencies]`, converts members to `workspace = true`
- **Prunes dead features** — removes features never enabled in the resolved graph
- **Fixes undeclared features** — adds missing feature declarations to member manifests
- **Detects unused deps** — flags dependencies not used anywhere (auto-removes on apply)
- **Computes MSRV** — derives minimum Rust version from dependency graph
- **Pins transitives** — replaces `cargo-hakari` without a workspace-hack crate

Multi-target aware: runs `cargo metadata` per target triple in parallel, computes feature *intersections* not unions.

### `split` / `sync`

Extract crates with full git history. Bidirectional sync with 3-way conflict resolution:

```bash
cargo rail split init crate/s         # configure extraction
cargo rail split run crate/s          # extract with history
cargo rail split run crate/s --check  # preview (dry-run)

cargo rail sync crate/s               # bidirectional sync
cargo rail sync crate/s --to-remote   # push changes to split repo
cargo rail sync crate/s --from-remote # pull changes (creates PR branch)
```

**Three modes:**

- `single` — one crate → one repo (most common)
- `combined` — multiple crates → one repo (shared utilities)
- `workspace` — multiple crates → workspace structure (mirrors monorepo)

Safety: refuses dirty worktree by default. `--allow-dirty` to override, `--yes` for CI.

### `release`

Dependency-order publishing with changelog generation:

```bash
cargo rail release check crate/s              # validate release readiness
cargo rail release run crate/s --bump minor   # bump, tag, publish
cargo rail release run crate/s --check        # preview release plan
```

Safety: detects default branch, refuses detached HEAD, warns on non-default branch.

### `config`

Manage configuration:

```bash
cargo rail init              # generate .config/rail.toml
cargo rail config locate     # print active config path
cargo rail config print      # print effective config with defaults
cargo rail config validate   # check for errors and unknown keys
cargo rail config sync       # update config with detected targets (incredibly useful on update)
```

---

## Configuration

Generated by `cargo rail init` at `.config/rail.toml`:

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
pin_transitives = false      # enable for hakari replacement
detect_unused = true
prune_dead_features = true

msrv = true
enforce_msrv_inheritance = false
msrv_source = "max"          # deps | workspace | max

[release]
tag_format = "{crate}-{prefix}{version}"
publish_delay = 5            # seconds between publishes

[change-detection]
infrastructure = [".github/**", "scripts/**", "*.sh"]
```

Full reference: [docs/config.md](docs/config.md)

---

## Real-World Results

Tested on production workspaces:

| Repo | Crates | Deps Unified | Dead Features |
|------|--------|--------------|---------------|
| **[tikv](https://github.com/tikv/tikv)** | 72 | 61 | 3 |
| **[meilisearch](https://github.com/meilisearch/meilisearch)** | 19 | 46 | 1 |
| **[helix](https://github.com/helix-editor/helix)** | 12 | 16 | 1 |
| **[tokio](https://github.com/tokio-rs/tokio)** | 10 | 10 | 0 |
| **[ripgrep](https://github.com/BurntSushi/ripgrep)** | 10 | 9 | 6 |
| **[polars](https://github.com/pola-rs/polars)** | 33 | 2 | 9 |
| **[ruff](https://github.com/astral-sh/ruff)** | 43 | 0 | 0 |

Demo recordings: [`examples/`](examples/)

---

## Migrating from cargo-hakari

Create a branch. Run `--check` first. Review the diff. This touches your entire workspace.

```bash
git checkout -b migrate-to-rail
rm -rf crates/workspace-hack
cargo rail init
# set pin_transitives = true in rail.toml
cargo rail unify --check        # review first
cargo rail unify                # apply
cargo check --workspace && cargo test --workspace
```

Full guide: [docs/migrate-hakari.md](docs/migrate-hakari.md)

---

## Design Decisions

**Resolution-based** — Uses Cargo's actual resolver output, not syntax parsing. If Cargo resolves it, cargo-rail sees it.

**Multi-target** — Runs `cargo metadata --filter-platform` per target in parallel. Computes feature *intersections*, not unions, w/ guardrails where it counts.

**System git** — Uses your `git` binary directly. No libgit2, no gitoxide. Deterministic SHAs.

**Lossless TOML** — Preserves comments and formatting via `toml_edit`.

**Minimal deps** — 11 direct dependencies. Built the release workflow specifically to avoid 200+ dep toolchains.

---

## FAQ

<details>
<summary><strong>How is this different from cargo-hakari?</strong></summary>

cargo-hakari creates a workspace-hack crate. cargo-rail writes unified versions directly to `[workspace.dependencies]` — no extra crate. Enable `pin_transitives = true` for equivalent behavior w/o the added CI check and lockfile steps.
</details>

<details>
<summary><strong>Does it work with workspace inheritance?</strong></summary>

Yes. Writes to `[workspace.dependencies]` and converts member manifests to `{ workspace = true }`.
</details>

<details>
<summary><strong>Virtual workspaces?</strong></summary>

Supported. For `pin_transitives`, `cargo-rail` auto-selects a workspace member as the transitive host (or configure `transitive_host` explicitly).
</details>

<details>
<summary><strong>Private registries?</strong></summary>

Works via `cargo metadata`, which respects `.cargo/config.toml`.
</details>

<details>
<summary><strong>Does this replace Bazel/Buck2 for Rust teams?</strong></summary>

For pure Rust workspaces, yes... it can. `cargo-rail` provides graph-aware testing, dependency unification, and crate extraction without learning a new build system. If you're using Bazel/Buck2 *only* for Rust (not polyglot builds), cargo-rail gives you the key benefits — affected analysis, hermetic builds via lockfiles, crate distribution — while staying in Cargo's ecosystem. I'm exploring the best way to build a proper cache feature (local will come first; remote will follow), as well.
</details>

<details>
<summary><strong>Why not just use cargo's built-in workspace features?</strong></summary>

Cargo workspaces are the foundation. cargo-rail adds what's missing: automatic version unification across the resolver's actual output, dead feature detection/pruning, MSRV computation from the dependency graph w/ options for how you use it, unused dep detection/removal, and graph-aware change detection. These require analysis Cargo doesn't do.
</details>

<details>
<summary><strong>How much CI time does this actually save?</strong></summary>

Depends on your workspace. In a 30-crate workspace where a PR touches 3 crates, you test 3 crates + their dependents instead of 50. I've seen 60-80% reductions in CI minutes for my own workspaces; teams with large workspaces and frequent, focused PRs will likely experience similar numbers.
</details>

<details>
<summary><strong>What are "undeclared features" and why should I care?</strong></summary>

Cargo unifies deps across your workspace. If `crate-a` depends on `serde` and `crate-b` depends on `serde` with `features = ["derive"]`, Cargo builds `serde` once with `derive` enabled for both. Now `crate-a` can use `#[derive(Serialize)]` even though it never declared that feature — it's "borrowing" from `crate-b`.

This works fine until: (1) you test `crate-a` in isolation, (2) you publish `crate-a`, or (3) `crate-b` drops the feature. Then `crate-a` breaks with cryptic compile errors.

`cargo rail unify` detects these borrowed features and auto-fixes them by adding the missing declarations to each crate's `Cargo.toml`. Cleaner graphs, safer publishes, tests that actually test what you ship.
</details>

---

## Documentation

- [Command Reference](docs/commands.md)
- [Config Reference](docs/config.md)
- [Migration Guide](docs/migrate-hakari.md)

---

## Contributing

Issues, PRs, and feedback welcome.

---

<p align="center">
  <a href="https://github.com/loadingalias/cargo-rail">GitHub</a> •
  <a href="https://crates.io/crates/cargo-rail">crates.io</a> •
  <a href="https://github.com/loadingalias/cargo-rail-action">GitHub Action</a>
</p>
