# cargo-rail

**Monorepo orchestration for Rust workspaces.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Commands](docs/commands.md) · [Config](docs/config.md) · [Recipes](docs/recipes.md) · [Crates.io](https://crates.io/crates/cargo-rail)

```bash
cargo install cargo-rail
```

---

## What It Does

**Test only what changed** — Graph-aware change detection. Auto-detects nextest. 70-90% CI reduction.

```bash
cargo rail test
```

**Unify dependencies** — Resolution-based `[workspace.dependencies]` without workspace-hack crates.

```bash
cargo rail unify --check    # Preview
cargo rail unify            # Apply
```

**Split crates with history** — Extract crates to standalone repos. Deterministic SHAs, git-notes mapping.

```bash
cargo rail split run my-crate
```

**Sync bidirectionally** — Monorepo ↔ split repo. Changes from external repos create PR branches.

```bash
cargo rail sync my-crate
```

**Release with ordering** — Dependency-ordered publishing. Native changelog generation.

```bash
cargo rail release run --all --bump minor --check
```

---

## Why cargo-rail

**11 dependencies, ~100 resolved.** Compare: cargo-hakari pulls ~40 direct via guppy. release-plz pulls 600+ resolved.

**Multi-target resolution.** Queries Cargo's actual resolver across all your target triples. Computes intersection of required features (not union).

**System git.** Uses `git` binary directly. No libgit2/gitoxide. Deterministic SHAs. git-notes for rebase-safe commit mapping.

**Lossless TOML.** Uses `toml_edit` to preserve comments and formatting.

---

## Commands

| Command | Purpose |
|---------|---------|
| `affected` | Show crates affected by changes |
| `test` | Run tests for affected crates only |
| `unify` | Unify deps to `[workspace.dependencies]` |
| `split` | Extract crate to standalone repo with history |
| `sync` | Bidirectional monorepo ↔ split repo sync |
| `release` | Version bump, changelog, tag, publish |
| `init` | Generate `rail.toml` |
| `check` | Validate release readiness |
| `clean` | Remove generated artifacts |
| `config` | Validate configuration |

Run `cargo rail <command> --help` for details, or see [docs/commands.md](docs/commands.md).

---

## Configuration

```bash
cargo rail init    # Generates .config/rail.toml
```

Searched in order: `rail.toml`, `.rail.toml`, `.cargo/rail.toml`, `.config/rail.toml`

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
pin_transitives = false    # Enable for hakari/workspace-hack replacement
prune_dead_features = true # Remove features never enabled in graph
detect_unused = true       # Find deps not in resolved graph
remove_unused = true       # Auto-remove unused deps
msrv = true                # Compute workspace MSRV from deps

[release]
tag_format = "{crate}-{prefix}{version}"
create_github_release = false
publish_delay = 5

[change-detection]
infrastructure = [".github/**", "justfile"]

[crates.my-crate.split]
remote = "git@github.com:org/my-crate.git"
branch = "main"
mode = "single"
```

Full reference: [docs/config.md](docs/config.md)

---

## Real-World Impact

`cargo rail unify` on tested monorepos:

| Repository | Crates | Deps Unified | Edits Saved |
|------------|--------|--------------|-------------|
| [tikv](https://github.com/tikv/tikv) | 83 | 57 | 516 |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 46 | 209 |
| [helix](https://github.com/helix-editor/helix) | 13 | 16 | 66 |
| [tokio](https://github.com/tokio-rs/tokio) | 10 | 10 | 35 |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 9 | 35 |

See [examples/](examples/) for full results across 12 repositories.

---

## Replaces

| Tool | cargo-rail equivalent |
|------|----------------------|
| cargo-hakari | `unify` with `pin_transitives = true` |
| cargo-udeps / cargo-machete | `detect_unused = true`, `remove_unused = true` |
| cargo-msrv | `msrv = true` |
| cargo-release / release-plz | `release` command |
| git-cliff | Built-in changelog generation |
| Copybara | `split` + `sync` commands |

---

## Installation

```bash
cargo install cargo-rail
```

From source:

```bash
git clone https://github.com/loadingalias/cargo-rail
cd cargo-rail && cargo install --path .
```

Requires Rust 1.91+.

---

## License

MIT

---

<sub>Built by [@loadingalias](https://github.com/loadingalias). Contributions welcome.</sub>
