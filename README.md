# cargo-rail

**Monorepo orchestration for Rust workspaces.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Commands](docs/commands.md) · [Config](docs/config.md) · [Examples](examples/) · [Crates.io](https://crates.io/crates/cargo-rail)

```bash
cargo install cargo-rail
```

Pre-built binaries available on the [Releases page](https://github.com/loadingalias/cargo-rail/releases).

---

## TLDR

- **Only test what changed both locally and in CI**: `cargo rail test`
- **Keep manifests clean and unified**: `cargo rail unify`
- **Split + sync crates with full git history**: `cargo rail split` / `cargo rail sync`
- **Release in dependency order with changelogs**: `cargo rail release`

Pure dev tooling. No workspace-hack crate. 11 direct dependencies.

---

## Quick Start

Drop into any Rust workspace:

```bash
cargo install cargo-rail
cargo rail unify --check
cargo rail unify --backup
cargo rail test
```

Optional configuration (auto-detected location):

```bash
cargo rail init                 # Generates .config/rail.toml by default
cargo rail config validate      # Sanity-check config
```

See [docs/commands.md](docs/commands.md)

---

## Workflows

### Change Detection & Testing

Only run tests for crates affected by your changes, with nextest auto-detection:

```bash
cargo rail affected              # Show affected crates
cargo rail test                  # Run tests for affected crates
cargo rail test --all            # Override and run the whole workspace
```

For CI, see [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action).

### Dependency Unification

Resolution-based `[workspace.dependencies]` management using Cargo’s resolver:

```bash
cargo rail unify --check         # Preview changes (CI-safe)
cargo rail unify                 # Apply changes
cargo rail unify --backup        # Apply with manifest backups; backup is automatic on the first 'unify' run
cargo rail unify undo            # Restore from a previous backup; default last
```

What it does, per target triple (`targets` in `rail.toml`):

- **Unifies versions** based on what Cargo actually resolved
- **Computes MSRV** from the resolved graph (`[workspace.package].rust-version`)
- **Prunes dead features** that are never enabled
- **Detects/removes unused deps** (opt-in via config)
- **Optionally pins transitives** (workspace-hack replacement)

### Split & Sync

Extract crates to standalone repos and keep them in sync, with deterministic SHAs.

Three modes: single crate to new repo, multiple crates to new repo, or multiple crates to a new workspace.

```bash
cargo rail split init crate              # Configure split for crate/s
cargo rail split run crate --check        # Preview the split
cargo rail split run crate               # Execute the split

cargo rail sync crate                    # Bidirectional sync
cargo rail sync crate --to-remote         # Monorepo -> split repo
cargo rail sync crate --from-remote       # Split repo -> monorepo (PR branch)
```

Split/sync behavior is driven by `[crates.NAME.split]` in `rail.toml`. See [docs/config.md](docs/config.md)

### Release

Version bumping, changelog generation, tagging, and publishing in dependency order:

```bash
cargo rail release init crate                    # Configure release
cargo rail release check crate                   # Fast validation
cargo rail release check crate --extended        # Dry-run + MSRV check
cargo rail release run crate --check             # Preview release plan
cargo rail release run crate --bump minor        # Execute release
```

Configure once in `rail.toml`:

```toml
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"         # Adjust for standalone crate
require_clean = true
```

---

## Example Videos

Real workspaces, recorded command workflows end-to-end. All assets live under [`examples/`](examples/).

<details>
<summary><strong>Unify: ripgrep</strong> - baseline unification in a widely trusted workspace</summary>

https://github.com/user-attachments/assets/93f34633-aa0e-4cde-8723-c81f3f474bac

</details>

<details>
<summary><strong>Unify: tokio</strong> - conservative settings for a core ecosystem library</summary>

https://github.com/user-attachments/assets/520abf55-cf45-43af-8dc8-0eed0a58ce72

</details>

<details>
<summary><strong>Unify: polars</strong> - large workspace with <code>pin_transitives</code> (cargo-hakari replacement)</summary>

https://github.com/user-attachments/assets/31bf5ff5-7185-4e59-acaa-ea8edd3c6f48

</details>

<details>
<summary><strong>Unify: helix-db</strong> - unification on a growing project</summary>

https://github.com/user-attachments/assets/3520d254-e69c-460c-b894-eb126b42a1ea

</details>

<details>
<summary><strong>Split: ruff</strong> - extracting crates with full git history</summary>

https://github.com/user-attachments/assets/b9f56e77-de0a-42c1-b2ef-1a40bb24f5ac

</details>

<details>
<summary><strong>Release: tikv</strong> - version bumping, changelog, and publishing in dependency order</summary>

https://github.com/user-attachments/assets/9c0b6df1-8539-44a0-9c82-a9fdca5e075c

</details>

---

## Unification Results in Testing/Validation

`cargo rail unify` on trusted Rust monorepos:

| Repository | Crates | Deps Unified | Member Edits |
|------------|--------|--------------|--------------|
| [tikv](https://github.com/tikv/tikv) | 83 | 57 | 519 |
| [polars](https://github.com/pola-rs/polars) | 33 | 2 | 13 |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 46 | 210 |
| [helix](https://github.com/helix-editor/helix) | 13 | 16 | 67 |
| [tokio](https://github.com/tokio-rs/tokio) | 10 | 10 | 35 |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 9 | 41 |
| [helix-db](https://github.com/helixdb/helix-db) | 6 | 16 | 44 |

Full demos and reports live under [`examples/`](examples/).

---

## Could Replace

| Tool | cargo-rail equivalent |
|------|-----------------------|
| cargo-hakari | `unify` with `pin_transitives = true` |
| cargo-udeps, cargo-machete, cargo-shear | `unify` with `detect_unused = true` / `remove_unused = true` |
| cargo-msrv (for dep-driven MSRV) | `unify` with `msrv = true` |
| cargo-release, release-plz | `release` command |
| git-cliff | Built-in changelog generation |
| Google's Copybara for Rust teams | `split` + `sync` commands |
| Mountain of shell scripts | `test` + `affected` + [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) in CI |

---

## Design Notes

I've built cargo-rail as a necessity for my own work. I've explained the reasoning behind it and touched on the plans for the future in this post: [Rust Monorepo Tooling](http://loadingalias.com/blog). I would love contributions; I'd love to iron out the details and workflows that we all use daily but that are clunky.

**Supply-Chain Safety**

This matters a great deal to me. I've tried to keep the tool itself lean; avoids large meta-dependency graphs. Currently, cargo-rail depends on 11 core deps; 77 resolved in the release build.

**Multi-Target Resolution**

Runs `cargo metadata --filter-platform` per target and computes feature intersection, not union. I often build for 7-9 target-triples; this was a requirement.

**System Git > Gix**

I've used the `git` binary directly for deterministic SHAs and proper history, no libgit2/gitoxide. They felt a bit heavy for the use case and we all use Git locally in some fashion.

**Lossless TOML**

Uses `toml_edit` so existing comments and layout are preserved. Manipulating TOML is honestly not as easy as it sounds. Please, if you run into any problems... open an 'Issue' and I'll take a look.

**Contributions Welcome**

---

<sub>Built by [@loadingalias](https://github.com/loadingalias)</sub>
