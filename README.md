# Rail

**The engine for Rust monorepos: run only what a change affects, reuse builds it can verify, keep dependencies
coherent, and ship releases in order.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

Rail loads the workspace Cargo actually resolved — not file paths, not guesses — and uses that one model to answer
every expensive question in a Rust monorepo: what does this change affect, which build results are still valid,
which dependency declarations have drifted, and what exactly ships next.

No hosted service. No second build language. No path-filter rules to maintain. Each capability works on its own —
adopt one at a time.

## Why teams use it

- **CI and local checks stop paying for the whole workspace.** `cargo rail plan` maps a change through the real
  dependency graph and selects only the crates, tests, and targets it can affect. A PR that touches one crate runs
  one crate's work — and `--explain` shows the reasoning for every selection and every skip.
- **Builds get faster without getting risky.** Rail's compiler cache reuses a result only after re-checking every
  input that produced it. Anything it cannot verify runs through Cargo normally, with the reason stated. Fast when
  it is safe, honest when it is not.
- **One tool replaces a shelf of plugins.** `cargo rail unify` keeps versions, features, workspace inheritance,
  unused dependencies, and MSRV consistent across the workspace — the job you would otherwise assemble from
  cargo-hakari, cargo-udeps, cargo-machete, cargo-msrv, and half a dozen more, each installing, updating, and
  re-parsing your workspace separately.
- **Releases are ordered, resumable, and explained to your users.** Contributors add short notes to `.changes/` as
  they work; Rail turns them into version bumps and changelogs, publishes crates dependency-first, and tags last.
  An interrupted release resumes from durable state instead of leaving half a release behind.
- **Develop in a monorepo, publish standalone.** `split` extracts a crate into its own repository with full git
  history; `sync` keeps the monorepo and the standalone repo in step afterward.

## Start with a plan

`plan` is read-only — it never runs actions or modifies tracked files, so it is safe to try on a real workspace:

```bash
cargo install cargo-rail
cargo rail plan --merge-base --explain
```

Then preview the exact commands Rail would run, before replacing anything in CI:

```bash
cargo rail run --merge-base --dry-run --print-cmd --explain
```

Keep your CI. Let Rail decide what needs to run:

```bash
cargo rail run --merge-base --profile ci
```

The built-in `ci` profile runs the selected build and test actions. Existing jobs can also consume Rail's scope
directly with `cargo rail plan --merge-base -f github`.

## Capabilities

- **Plan** — Select affected work from Cargo's resolved dependency graph instead of path globs. Use the
  [planner machine contract](docs/planner-contract.md) from your own jobs, or let `cargo rail run` execute the
  selected scope.
- **Cache** — Reuse compiler results only after full revalidation of their inputs. The generated
  [support matrix](docs/cache-capabilities.md) lists exactly which work is cached and which runs cold, and
  [the caching guide](docs/caching.md) covers method, scorecards, and limitations.
- **Unify** — Repair dependency versions, features, unused edges, workspace inheritance, and MSRV from Cargo
  resolution plus compiler evidence. Start with `cargo rail unify --check --explain`.
- **Change + Release** — Record user-facing changes as small `.changes/*.md` files (the "changesets" workflow,
  for Cargo workspaces), then release the exact reviewed commit: readiness checks, dependency-ordered publishing,
  tags last, and resume on interruption.
- **Split + Sync** — Extract a crate with its history, then map later changes in both directions between the
  monorepo and the standalone repository. Follow the [split/sync example](examples/split-sync/README.md).

## Install and initialize

```bash
cargo install cargo-rail
cargo rail init --dry-run
cargo rail init
cargo rail config validate --strict
```

Pre-built archives for x86-64/ARM64 Linux (GNU and musl), Windows MSVC, and macOS — with SHA-256 checksums and
signed provenance — are on [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases).
`cargo-binstall cargo-rail` is supported.

After upgrading, run `cargo rail config migrate --check`. Exit 1 means a config migration is available: review it,
apply it with `cargo rail config migrate`, then run `cargo rail config validate --strict`.

## What Rail replaces

Each tool below rebuilds its own partial picture of your workspace. Rail captures Cargo resolution, source state,
and git history once, and every command works from that shared model:

| Replace | With |
|---|---|
| `cargo-hakari`, dependency-unification scripts | `cargo rail unify` |
| `cargo-udeps`, `cargo-shear`, `cargo-machete` | Compiler-backed unused-dependency detection and removal |
| `cargo-unused-features`, `cargo-features-manager`, `cargo-autoinherit`, `cargo-msrv`, feature-audit scripts | Dead-feature pruning, borrowed-feature repair, inheritance enforcement, and MSRV computation |
| `release-plz`, `cargo-release`, `git-cliff` | `cargo rail change` + `cargo rail release` |
| `dorny/paths-filter`, path globs, package-selection scripts | `cargo rail plan` + `cargo rail run` |
| Hand-maintained publish ordering | Dependency-ordered workspace releases |
| Copybara, `git subtree`, split/sync scripts | `cargo rail split` + `cargo rail sync` |

- [Migrate from cargo-hakari](docs/migrate-hakari.md)
- [Migrate from git-cliff or release-plz](docs/migrate-git-cliff.md)

## Reference

- [Command reference](docs/commands.md)
- [Configuration reference](docs/config.md)
- [Change detection](docs/change-detection.md)
- [Caching method, scorecards, and limitations](docs/caching.md)
- [Generated execution, cache, and performance support matrix](docs/cache-capabilities.md)
- [Architecture](docs/architecture.md)
- [Planner machine contract](docs/planner-contract.md)
- [Troubleshooting and recovery](docs/troubleshooting.md)

## Project

Rail is actively maintained. The `cargo-rail` crate requires Rust 1.97.1 or newer and is licensed under
[MIT](LICENSE).

- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Issue tracker](https://github.com/loadingalias/cargo-rail/issues)
