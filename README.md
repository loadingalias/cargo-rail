# Cargo-Rail

**A Rust Workspace Engine. Install One Tool. Delete the Glue.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

Rust workspaces accumulate path filters, dependency auditors, build cache glue, release bots, and split repo scripts. Each tool owns a partial model of the same codebase; computational waste piles up fast. Cargo-Rail replaces that tool pile with one Cargo-native binary built around the workspace Cargo actually resolved.

**Cargo-Rail does not replace Cargo or nextest. It makes them do less. In some cases, a lot less.** It keeps dep declarations coherent, selects work from the resolved graph, reuses only compiler results it can prove valid, turns reviewed change intent into exact-SHA releases, and keeps standalone crates sync'd with their monorepo source.

No hosted service. No second build language. No hand-rolled crate path filters. Adopt one workflow at a time.

## One binary, not a bundle

Cargo-Rail implements these workflows around one captured workspace view. It does not coordinate the maintenance tools below behind the scenes.

| Retire | Use |
|---|---|
| `cargo-hakari` and dependency-unification scripts | `cargo rail unify` |
| `cargo-udeps`, `cargo-shear`, `cargo-machete`, feature auditors, and MSRV scripts | Compiler-backed dependency and feature analysis |
| `dorny/paths-filter`, path globs, and package-selection scripts | `cargo rail plan` + `cargo rail run` |
| `release-plz`, `cargo-release`, `git-cliff`, and publish-order scripts | `cargo rail change` + `cargo rail release` |
| Copybara or custom scripts for private-monorepo to public-crate sync | `cargo rail split` + `cargo rail sync` |

The effect compounds: one tool removes orchestration overhead; a coherent graph removes proven waste and hidden coupling; affected planning removes irrelevant actions; verified reuse removes compiler work from the actions that remain; release and sync remove recovery-prone automation at repo boundaries.

- [Migrate from cargo-hakari](docs/migrate-hakari.md)
- [Migrate from git-cliff or release-plz](docs/migrate-git-cliff.md)

## Start with proof

Install Cargo-Rail, then inspect a real branch. `plan` does not execute the selected actions or edit tracked files:

```bash
cargo install cargo-rail
cargo rail plan --merge-base --explain
```

Preview the exact commands before changing CI:

```bash
cargo rail run --merge-base --dry-run --print-cmd --explain
```

Then keep your CI and let Cargo-Rail select its scope:

```bash
cargo rail run --merge-base --profile ci
```

The built-in `ci` profile runs the selected build and test actions. Existing jobs can consume the same scope through `cargo rail plan --merge-base -f github`.

Pre-built archives, SHA-256 checksums, and signed provenance are available from [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases). `cargo-binstall cargo-rail` is also supported.

## Less work, in the right order; it compounds.

1. **Make the graph coherent.** `cargo rail unify` detects version drift, hidden feature coupling, unused dependency edges, workspace-inheritance drift, and MSRV mismatches. Graph-removing decisions carry compiler evidence, and Cargo-Rail validates the resulting Cargo graph before lossless TOML edits. Published packages keep their open-world feature surface unless config explicitly authorizes closed-workspace cleanup. Start with `cargo rail unify --check --explain`. Apply reversibly with `cargo rail unify --backup`; restore the latest backup with `cargo rail unify undo`.

2. **Run only affected work.** `cargo rail plan` maps changed files to owning crates, walks reverse deps, and selects build, test, documentation, benchmark, infrastructure, and configured repository actions. `--explain` prints the reason chain. Text, JSON, GitHub output, dry-run previews, and `cargo rail run` consume the same plan.

3. **Reuse only verified compiler results.** Cargo-Rail revalidates the toolchain, sources, dependency artifacts, environment reads, action identity, stored result, and exact output blobs before a native cache hit. Unsupported or incomplete cases run through Cargo with a named bypass. **Fast when proven. Normal Cargo when not.**

   The checked-in 110-sample M1 Pro fixture (it's my personal dev machine) measured warm cross-root p50 reductions of **29.2% for checks** and **26.4% for release builds** against native Cargo. These are same-host fixture results, not universal performance claims. The default native CAS lives outside `target/`, so `cargo clean` does not erase reusable results. See the [caching method and measurements](docs/caching.md) and generated [support matrix](docs/cache-capabilities.md) for the exact graduated classes and current limits.

4. **Record release intent before history loses it.** Contributors add small `.changes/*.md` files with the code they change. That reviewed intent is the default source for version bumps and release notes; commit-derived release modes remain available for compatibility. Cargo-Rail validates the exact release commit, waits for readiness on that SHA, publishes in dependency order, observes each registry result, and creates tags after publication. Durable state makes an interrupted release resumable instead of ambiguous.

5. **Keep public crates tied to their monorepo source.** For the private-monorepo to public Rust crate workflow, `split` extracts the relevant Git history and rewrites workspace-relative manifests. `sync` maps later commits in both directions, creates inbound review branches, uses Git three-way merge, and records manual conflicts in resumable receipts. This is Cargo-aware repository synchronization, not a general-purpose transformation DSL.

## Reference

- [Command reference](docs/commands.md)
- [Config reference](docs/config.md)
- [Change detection](docs/change-detection.md)
- [Architecture](docs/architecture.md)
- [Planner machine contract](docs/planner-contract.md)
- [Split/sync example](examples/split-sync/README.md)
- [Troubleshooting and recovery](docs/troubleshooting.md)

## Project

Cargo-Rail is licensed under [MIT](LICENSE). The current MSRV is published in
[`Cargo.toml`](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml).

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Issue tracker](https://github.com/loadingalias/cargo-rail/issues)
