# cargo-rail

> Rust monorepo tooling for change detection, graph unification, release automation, and split/sync.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## What It Covers

- `plan` / `run`: file-first change detection for selective build, test, bench, docs, and infra execution
- `unify`: workspace dependency unification, feature cleanup, unused dependency detection, and MSRV derivation
- `release`: version bump, changelog generation, tagging, and publish flow without a large external toolchain
- `split` / `sync`: copybara-style crate extraction and bidirectional sync without a separate DSL

## Quick Start

```bash
cargo install cargo-rail

cargo rail init
cargo rail unify --check
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci
```

Pre-built binaries: [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases)

## Core Workflows

### Change Detection

Use `plan` to build the deterministic contract and `run` to execute only the selected work.

```bash
cargo rail plan --merge-base
cargo rail plan --merge-base -f github
cargo rail run --merge-base --profile ci
```

`impact` is diagnostic. `scope` is the execution handoff.

### Graph Unification

Use `unify` to keep the workspace dependency graph lean and consistent.

```bash
cargo rail unify --check
cargo rail unify --check --explain
cargo rail unify
```

### Release Workflow

Use `release` for checks, changelog generation, tags, and publish ordering.

```bash
cargo rail release check
cargo rail release run cargo-rail --bump patch --yes
```

### Split / Sync

Use `split` and `sync` when a crate needs to live in both a monorepo and a standalone repository.

```bash
cargo rail split init crates/my-crate
cargo rail split run crates/my-crate
cargo rail sync crates/my-crate --to-remote
```

## GitHub Actions

Use [cargo-rail-action](https://github.com/loadingalias/cargo-rail-action) for planner gates and execution scope in GitHub Actions.

## Config

```bash
cargo rail init
cargo rail config sync
cargo rail config validate
```

Primary references:

- [Configuration Reference](docs/config.md)
- [Command Reference](docs/commands.md)
- [Change Detection Guide](docs/change-detection.md)
- [Architecture](docs/architecture.md)

## Migration

- [Migrate from `cargo-hakari`](docs/migrate-hakari.md)

## Getting Help

- Issues: [GitHub Issues](https://github.com/loadingalias/cargo-rail/issues)
- Crate: [crates.io/cargo-rail](https://crates.io/crates/cargo-rail)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

See [SECURITY.md](SECURITY.md).

## License

Licensed under [MIT](LICENSE).
