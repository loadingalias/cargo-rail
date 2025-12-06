# cargo-rail

**Monorepo orchestration for Rust workspaces.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Command Reference](docs/commands.md) · [Config Reference](docs/config.md) · [Demo Videos](examples/)

```bash
cargo install cargo-rail
```

Pre-built binaries available on the [Releases Page](https://github.com/loadingalias/cargo-rail/releases).

---

## TLDR

- **Only test what changed both locally and in CI**: `cargo rail test`
- **Keep manifests clean and unified**: `cargo rail unify` / `cargo rail unify sync`
- **Split + sync crates with full git history**: `cargo rail split` / `cargo rail sync`
- **Release in dependency order with changelogs**: `cargo rail release`
- **Minimal deps**: 11 core dependencies; 77 resolved dependencies

---

## Quick Start

Drop into any Rust workspace:

```bash
cargo install cargo-rail
cargo rail init                               # Initialize once
cargo rail config validate                    # Validate config
cargo rail unify --check                      # Dry-run 'unify'
cargo rail unify                              # Initial 'unify' automatically writes backup
cargo rail test                               # Auto-detect Nextest; native change-detection
```

See [docs/commands.md](docs/commands.md) for details.

---

## Workflows

### Change Detection & Testing

Graph-aware change detection. Only check/test/bench what's affected:

```bash
cargo rail affected                           # Show affected crates
cargo rail affected -f names-only             # Just names (for scripting)
cargo rail test                               # Run tests for affected crates
cargo rail test --all                         # Override and run the whole workspace
```

Wire it into your local workflow:

```bash
# scripts/check.sh (or justfile)
AFFECTED=$(cargo rail affected -f names-only 2>/dev/null || echo "")
if [ -z "$AFFECTED" ]; then
  cargo check --workspace --all-targets
else
  for crate in $AFFECTED; do FLAGS="$FLAGS -p $crate"; done
  cargo check $FLAGS --all-targets
fi
```

For CI, see [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action).

### Dependency Unification

Resolution-based `[workspace.dependencies]` management using Cargo's resolver:

```bash
cargo rail unify --check         # Preview changes (CI-safe, exits 1 if changes needed)
cargo rail unify                 # Apply changes
cargo rail unify --backup        # Apply with manifest backups
cargo rail unify undo            # Restore from a previous backup
cargo rail unify sync            # Re-detect targets and merge into rail.toml
```

What it does, per target triple (`targets` in `rail.toml`):

- **Unifies versions** based on what Cargo actually resolved
- **Computes MSRV (dependencies)** from the resolved graph (`[workspace.package].rust-version`)
- **Prunes dead features** that are never enabled
- **Detects/removes unused deps** (opt-in via config)
- **Optionally pins transitives** (`cargo-hakari` or workspace-hack replacement)

Use `unify sync` in pre-commit hooks or CI to enforce a lean, consistent dependency graph or after adding new target triples to your CI matrix or `.cargo/config.toml`.

### Split & Sync

Extract crates to standalone repos and keep them in sync, with deterministic SHAs.

Three modes: single crate to new repo, multiple crates to new repo, or multiple crates to a new workspace.

```bash
cargo rail split init crate                   # Configure split for crate/s
cargo rail split run crate --check            # Preview the split
cargo rail split run crate                    # Execute the split

cargo rail sync crate                         # Bidirectional sync
cargo rail sync crate --to-remote             # Monorepo -> split repo
cargo rail sync crate --from-remote           # Split repo -> monorepo (PR branch)
```

Split/sync behavior is driven by `[crates.NAME.split]` in `rail.toml`. 3-way conflict resolution is configurable in `rail.toml` once a split has been initialized. See [docs/config.md](docs/config.md)

### Release

Version bumping, changelog generation, tagging, and publishing in dependency order:

```bash
cargo rail release init crate                 # Configure release
cargo rail release check crate                # Fast validation
cargo rail release check crate --extended     # Dry-run + MSRV check
cargo rail release run crate --check          # Preview release plan
cargo rail release run crate --bump minor     # Execute release
```

Configure once in `rail.toml`:

```toml
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"      # Adjust for standalone crate
require_clean = true
```

---

## Demo Videos

I've tested across trusted Rust workspaces and recorded the command workflows end-to-end. All assets live under [`examples/`](examples/).

<details>
<summary><strong>ripgrep</strong> - baseline unification in a widely trusted workspace</summary>

https://github.com/user-attachments/assets/93f34633-aa0e-4cde-8723-c81f3f474bac

</details>

<details>
<summary><strong>tokio</strong> - conservative settings for a core ecosystem library</summary>

https://github.com/user-attachments/assets/520abf55-cf45-43af-8dc8-0eed0a58ce72

</details>

<details>
<summary><strong>polars</strong> - large workspace with <code>pin_transitives</code> (cargo-hakari replacement)</summary>

https://github.com/user-attachments/assets/31bf5ff5-7185-4e59-acaa-ea8edd3c6f48

</details>

<details>
<summary><strong>helix-db</strong> - unification on a growing project</summary>

https://github.com/user-attachments/assets/3520d254-e69c-460c-b894-eb126b42a1ea

</details>

<details>
<summary><strong>ruff</strong> - extracting crates with full git history</summary>

https://github.com/user-attachments/assets/b9f56e77-de0a-42c1-b2ef-1a40bb24f5ac

</details>

<details>
<summary><strong>tikv</strong> - version bumping, changelog, release, and publishing in dep order</summary>

https://github.com/user-attachments/assets/9c0b6df1-8539-44a0-9c82-a9fdca5e075c

</details>

---

## Unification Results in Testing/Validation

`cargo rail unify` on a few different projects:

| Repo | Crates | Deps Unified | Member Edits |
|------------|--------|--------------|--------------|
| [tikv](https://github.com/tikv/tikv) | 83 | 57 | 519 |
| [polars](https://github.com/pola-rs/polars) | 33 | 2 | 13 |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 46 | 210 |
| [helix](https://github.com/helix-editor/helix) | 13 | 16 | 67 |
| [tokio](https://github.com/tokio-rs/tokio) | 10 | 10 | 35 |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 9 | 41 |
| [helix-db](https://github.com/helixdb/helix-db) | 6 | 16 | 44 |

Full demos and reports live under [`examples/`](examples/) for most examples.

---

### Potentially Replaces

| Tool | cargo-rail equivalent |
|------|-----------------------|
| `cargo-hakari` | `unify` with `pin_transitives = true` |
| `cargo-udeps` / `cargo-machete` / `cargo-shear` | `unify` with `detect_unused = true` / `remove_unused = true` |
| `cargo-msrv` (for dep-driven MSRV)* | `unify` with `msrv = true` |
| `cargo-release` / `release-plz` | `release` command |
| `git-cliff` | Built-in changelog generation |
| Google's `Copybara` for Rust teams | `split` + `sync` commands |
| Mountain of shell scripts | `test` + `affected` + [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) in CI |

#### NOTE ON MSRV

The `msrv=true` configuration doesn't mean `cargo-rail` runs compilation checks. Instead, it computes the floor msrv for the workspace, and takes your own `rust-version` into consideration. This allows you to immediately determine the msrv for your workspace based on the dependencies you're using and your own `rust-version`.

---

## Design Notes

I've built cargo-rail as a necessity for my own work and for that reason it's opinionated. I've explained the thought process behind it and touched on the plans for the future in this post: [Coming Soon](http://loadingalias.com/blog/). I am absolutely hoping to get the community involved in improving the tooling here; contributions are welcome!

**Supply-Chain Safety**

This matters a great deal to me. I've tried to keep the tool itself lean; it deliberately avoids large meta-dependency graphs. Currently, cargo-rail depends on 11 core deps; 77 resolved in the release build.

**Multi-Target Resolution**

cargo-rail runs `cargo metadata --filter-platform` per target-triple and computes feature intersection, not union... with guardrails in place, obviously. I often build for 6-9 target-triples... so this was a requirement.

**System Git > Gix**

I've used the `git` binary directly for deterministic SHAs and proper history, no `libgit2` / `gitoxide`. They felt a bit heavy for this use case and I'm assuming we all use git locally in some fashion anyway.

**Lossless TOML**

I've used `toml_edit` so that existing comments and layout are preserved. Manipulating TOML is not as straightforward as it sounds. Please, if you run into any problems... open an 'Issue' and I'll take a look... or submit a PR.

**Contributions Welcome**

---

<sub>Built by [@loadingalias](https://github.com/loadingalias)</sub>
