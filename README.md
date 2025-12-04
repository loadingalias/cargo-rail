# cargo-rail

> **⚠️ WORK IN PROGRESS - NOT CANONICAL ⚠️**
>
> This project is under active development. APIs, configuration options, and behaviors may change without notice.

**Monorepo orchestration for Rust workspaces.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Commands](docs/commands.md) | [Config](docs/config.md) | [Examples](examples/) | [Crates.io](https://crates.io/crates/cargo-rail)

```bash
cargo install cargo-rail
```

---

## What It Does

**Test only what changed** - Graph-aware change detection. Auto-detects nextest.

```bash
cargo rail test
```

**Unify dependencies** - Resolution-based `[workspace.dependencies]` management.

```bash
cargo rail unify --check    # Preview
cargo rail unify            # Apply
```

**Split crates with history** - Extract crates to standalone repos with deterministic SHAs.

```bash
cargo rail split run my-crate
```

**Sync bidirectionally** - Monorepo to split repo and back. External changes create PR branches.

```bash
cargo rail sync my-crate
```

**Release with ordering** - Dependency-ordered publishing with changelog generation.

```bash
cargo rail release run --all --bump minor
```

---

## How It Works

cargo-rail runs `cargo metadata --filter-platform` for each target triple in your workspace, then computes:

1. **Resolved versions** - What Cargo's resolver actually picked, not declared versions
2. **Feature sets** - Intersection for workspace deps, union fallback for edge cases
3. **MSRV floor** - Maximum `rust-version` from the entire resolved graph
4. **Unused deps** - Declared but absent from resolution
5. **Dead features** - Declared but never enabled anywhere

One metadata pass per target. No additional cargo invocations. No workspace-hack crate.

---

## Commands

| Command | Purpose |
|---------|---------|
| `affected` | Show crates affected by changes |
| `test` | Run tests for affected crates only |
| `unify` | Unify deps to `[workspace.dependencies]` |
| `split` | Extract crate to standalone repo with history |
| `sync` | Bidirectional monorepo/split repo sync |
| `release` | Version bump, changelog, tag, publish |
| `init` | Generate `rail.toml` |
| `clean` | Remove generated artifacts |
| `config` | Configuration management |

See [docs/commands.md](docs/commands.md) for full reference.

---

## Configuration

```bash
cargo rail init    # Generates .config/rail.toml
```

Searched in order: `rail.toml`, `.rail.toml`, `.cargo/rail.toml`, `.config/rail.toml`

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true

[release]
tag_format = "{crate}-v{version}"

[change-detection]
infrastructure = [".github/**", "justfile"]
```

See [docs/config.md](docs/config.md) for full reference.

---

## Tested On

`cargo rail unify` results on real monorepos:

| Repository | Crates | Deps Unified | Member Edits |
|------------|--------|--------------|--------------|
| [tikv](https://github.com/tikv/tikv) | 83 | 57 | 516 |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 46 | 209 |
| [helix](https://github.com/helix-editor/helix) | 13 | 16 | 66 |
| [tokio](https://github.com/tokio-rs/tokio) | 10 | 10 | 35 |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 9 | 35 |

See [examples/](examples/) for demo videos and full results across 12 repositories.

---

## Replaces

| Tool | cargo-rail equivalent |
|------|----------------------|
| cargo-hakari | `unify` with `pin_transitives = true` |
| cargo-udeps, cargo-machete, cargo-shear | `detect_unused = true` |
| cargo-msrv (for dep-driven MSRV) | `msrv = true` |
| cargo-release, release-plz | `release` command |
| git-cliff | Built-in changelog generation |
| Copybara | `split` + `sync` commands |

---

## Design

**11 dependencies, ~100 resolved.** cargo-hakari pulls ~40 direct via guppy. release-plz pulls 600+ resolved.

**Multi-target resolution.** Queries Cargo's resolver per target triple. Computes feature intersection, not union.

**System git.** Uses `git` binary directly. No libgit2/gitoxide. Deterministic SHAs. git-notes for commit mapping.

**Lossless TOML.** Uses `toml_edit` to preserve comments and formatting.

---

## License

MIT

---

<sub>Built by [@loadingalias](https://github.com/loadingalias). Contributions welcome.</sub>
