# cargo-rail

> Replace the Rust workspace maintenance stack with one graph-aware Cargo tool.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

`cargo-rail` unifies dependencies, selects affected CI work, releases crates, and maintains standalone crate repositories from the same Cargo graph and git history.

One binary. One `rail.toml`. No hosted service, workspace-hack crate, embedded git implementation, or separate workflow runtime.

## Replace the stack

| Replace | With |
| --- | --- |
| `cargo-hakari`, dependency-unification scripts | `cargo rail unify` |
| `cargo-udeps`, `cargo-shear`, `cargo-machete` | Built-in compiler-backed unused-dependency detection and removal |
| `cargo-unused-features`, `cargo-features-manager`, `cargo-autoinherit`, `cargo-msrv`, feature-audit scripts | Dead-feature pruning, borrowed-feature repair, inheritance enforcement, and MSRV computation |
| `release-plz`, `cargo-release`, `git-cliff` | `cargo rail change` + `cargo rail release` |
| `dorny/paths-filter`, path globs, package-selection scripts | `cargo rail plan` + `cargo rail run` |
| Hand-maintained publish ordering | Dependency-ordered workspace releases |
| Copybara, `git subtree`, split/sync scripts | `cargo rail split` + `cargo rail sync` |

These are not disconnected wrappers. Every workflow consumes the same resolved workspace model, so a dependency change can select CI work, drive a release, order publication, and remain traceable into a standalone repository.

## Install

```bash
cargo install cargo-rail
cargo rail init
cargo rail config validate --strict
```

Pre-built archives for Linux, Windows, and Apple Silicon macOS, SHA-256 checksums, and signed provenance are available from [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases). `cargo-binstall cargo-rail` is supported.

After an upgrade, run `cargo rail config migrate --check`. Exit 1 means an explicit semantic migration is available; review it, apply it with `cargo rail config migrate`, then validate. Coded defaults are never copied into `rail.toml`.

## Unify

Make workspace manifests agree. `unify` detects version drift, fragmented features, unused dependencies, dead features, undeclared feature borrowing, and MSRV mismatches. It can replace a generated workspace-hack crate with explicit workspace dependencies.

Unused-edge decisions combine Cargo's resolved graph with workspace-only rustc evidence across configured targets and source-derived feature selections. Results are cached by compilation-unit identity, and apply revalidates the exact manifest edits and portable graph delta before committing them. Destructive cleanup of dormant private features and optional dependencies requires `consumer_scope = "workspace"`; published packages remain open-world.

```bash
cargo rail unify --check --explain  # inspect every decision
cargo rail unify doctor             # inspect resolver/target/alias hazards
cargo rail unify                    # apply the plan
cargo rail unify undo               # restore the last backup
```

The check path writes nothing and exits non-zero when manifests need changes.

## Change detection

Turn git changes into an executable Cargo scope. `plan` maps files to owning crates, walks reverse dependencies, classifies build, test, docs, bench, and infrastructure surfaces, then emits a stable machine-readable contract.

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci
```

Use `run` directly or feed `plan -f github` into existing CI jobs. No duplicated package-selection logic and no path-filter graph to keep synchronized with Cargo.

## Release

Record release intent in the pull request that introduces the change. `release` combines reviewed change files with conventional commits, dependency cascades, changelog generation, readiness checks, publish ordering, tags, and forge releases.

```bash
cargo rail change add my-crate --bump minor --message "Added graph-aware planning."
cargo rail change check --merge-base --required
cargo rail release run --all --bump auto --pr --check
```

After the release PR merges:

```bash
cargo rail release finalize --all --yes
```

Interrupted releases are resumable. Optional `cargo-semver-checks` analysis runs as an external command instead of expanding cargo-rail's installed dependency graph.

## Split and sync

Develop crates in a monorepo and publish them from focused standalone repositories. `split` filters crate history and rewrites workspace-relative manifests; `sync` maps later commits in either direction.

```bash
cargo rail split init my-crate
cargo rail split run my-crate --check
cargo rail split run my-crate
cargo rail sync my-crate --to-remote
```

Conflicts use explicit `manual`, `ours`, `theirs`, or `union` strategies with resumable receipts. Copybara configuration and subtree choreography are unnecessary.

## Why one tool?

Cargo already owns the package graph. Git already owns change history. cargo-rail uses those sources directly instead of rebuilding partial models in a collection of plugins, actions, bots, and shell scripts.

The result is one install graph, one configuration surface, and one set of decisions to inspect. See the [architecture](docs/architecture.md) for the internal model and the [planner contract](docs/planner-contract.md) for its stable CI interface.

## Commands

| Command | Purpose |
| --- | --- |
| `init` | Create or update `rail.toml` |
| `unify` | Inspect and repair workspace dependency state |
| `plan` / `run` | Select and execute work affected by a change |
| `change` / `release` | Review release intent and run graph-ordered releases |
| `split` / `sync` | Maintain standalone repositories from monorepo crates |
| `config`, `graph`, `hash` | Validate configuration and explain planner state |

## Documentation

- [Configuration reference](docs/config.md)
- [Command reference](docs/commands.md)
- [Change detection](docs/change-detection.md)
- [Planner machine contract](docs/planner-contract.md)
- [Migrate from cargo-hakari](docs/migrate-hakari.md)
- [Migrate from git-cliff or release-plz](docs/migrate-git-cliff.md)
- [Split/sync example](examples/split-sync/README.md)

## Project

cargo-rail is actively maintained, requires Rust 1.95.0 or newer, and is licensed under [MIT](LICENSE).

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Issue tracker](https://github.com/loadingalias/cargo-rail/issues)
