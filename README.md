# cargo-rail

> Use Cargo metadata and git history to plan CI, keep workspace dependencies consistent, release crates, and maintain standalone crate repositories.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## What cargo-rail changes

Cargo knows which crates depend on each other. Git knows which files changed. CI path filters, release scripts, and subtree jobs usually rebuild parts of that information independently.

`cargo-rail` reads both sources once and uses the result across CI, dependency maintenance, releases, and crate extraction. It adds one repository file, `rail.toml`, plus optional reviewed change files under `.changes/`. It does not add a daemon, hosted service, build system, or workspace-hack crate.

| Existing workflow | What changes |
| --- | --- |
| GitHub path filters or shell conditionals | `plan` maps changed files to crates, then includes affected dependents |
| CI scripts choose packages independently | `plan` emits one scope; `run` or another CI step consumes it |
| Release tools infer intent from commits | `change` records the crate, bump, and release note in the pull request that introduced the change |
| Release scripts order crates by hand | `release` orders publishable crates from Cargo's dependency graph |
| Dependency versions and features drift between manifests | `unify` reports or applies one workspace-level declaration and checks feature/MSRV problems |
| `cargo-hakari` maintains a workspace-hack crate | Optional transitive pinning writes explicit workspace dependencies instead |
| Copybara, `git subtree`, or extraction scripts | `split` rewrites a crate into a standalone repository; `sync` maps later commits in either direction |

Mutation workflows provide check or planning commands before apply where the operation supports it. The check path reports required changes without writing them.

## Four workflows, one Cargo graph

The individual commands replace separate tools, but the larger gain comes from sharing one model of the workspace. Dependency cleanup changes the graph used by CI and releases. Change detection selects the same crates that release attribution and publish ordering use. Split repositories retain a mapping back to the monorepo that produced them.

### Dependency unification

`unify` works from resolved Cargo metadata across configured targets. It finds version drift, fragmented transitive features, unused dependencies, dead features, undeclared feature borrowing, and MSRV mismatches. Apply mode rewrites manifests with shared workspace dependencies and can replace a `cargo-hakari` workspace-hack crate with explicit transitive pins.

Removing duplicate versions, unused edges, and unnecessary features reduces the graph Cargo must resolve, download, compile, audit, and track. The exact build-time reduction depends on which dependencies unify; `cargo rail unify --check --explain` shows every proposed decision before changing a manifest.

### Reviewed releases

`change` brings the changesets workflow to Cargo: the pull request that changes a crate also records its bump and release note. `release` combines those reviewed files with conventional commits, optional `cargo-semver-checks` signals, and dependency cascades. It then updates manifests and changelogs, creates the release PR, orders publishes by dependency, creates tags and forge releases, and records resumable state.

The release path does not embed `release-plz`, git-cliff, a template engine, or a hosted bot. Graph-based attribution replaces per-crate path globs, and built-in changelog grouping covers the normal customization path without adding another release tool's dependency graph.

### Monorepo development, standalone OSS repositories

`split` filters crate history and rewrites workspace-relative manifests into a clean standalone repository. `sync` maps later commits between that repository and the monorepo. Concurrent edits use a three-way merge with explicit `manual`, `ours`, `theirs`, and `union` conflict strategies plus resumable receipts.

This keeps the monorepo as the development home while each public crate can ship from a focused repository with its own issues, releases, CI, and contributor surface.

### CI scope from code impact

`plan` maps changed files to owning crates, walks reverse dependencies, classifies build/test/docs/infra surfaces, and emits a stable execution scope. `run` consumes that scope, or existing CI jobs can use the emitted Cargo package arguments.

On the maintainer's repositories, replacing broad CI runs with planner-selected work reduced CI execution by roughly 50–70%. That is an observed result, not a fixed benchmark: savings depend on workspace shape, change locality, and the amount of work currently run on every commit. `cargo rail plan --merge-base --explain` shows the selected work before a team changes its CI gates.

## Supply-chain footprint

The current crate has 14 normal direct dependencies and 74 unique non-root package versions in its locked normal dependency graph. It uses Cargo metadata and the system `git` executable instead of embedding a build engine, git implementation, workflow runtime, or changelog template stack.

Release archives are checksum-verified, smoke-tested on their native architectures, and published with signed provenance. GitHub Actions are pinned by commit SHA, dependency policy is checked in CI, and optional `cargo-semver-checks` analysis runs as an external command rather than expanding cargo-rail's installed graph.

## Repository fit

For a multi-crate workspace, cargo-rail can connect a change in a library crate to dependent tests, release bumps, publish order, and standalone mirrors. This is the main use case.

For a single crate, `plan`, `run`, dependency checks, change files, changelog generation, and release commands still work. The graph-aware selection and multi-crate publish ordering collapse to one package, so a small repository with basic CI may get the same result from Cargo and a shorter release script.

For a polyglot monorepo, cargo-rail models the Rust workspace and can expose `infra` or custom CI surfaces for other files. It does not replace a language-agnostic build graph such as Bazel, Buck2, or Moon.

## Quick Start

```bash
cargo install cargo-rail

cargo rail init
cargo rail unify --check
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci
```

Each [GitHub Release](https://github.com/loadingalias/cargo-rail/releases) includes pre-built archives for the targets in
[distribution/release-targets.json](https://github.com/loadingalias/cargo-rail/blob/main/distribution/release-targets.json).
Release CI smoke-tests each archive on its native architecture and publishes SHA-256 checksums and signed provenance.

## Command map

| Command | Purpose |
| --- | --- |
| `plan` / `run` | Selective build, test, bench, docs, and infra execution from one deterministic plan |
| `unify` | Workspace dependency unification, feature cleanup, unused dependency detection, and MSRV derivation |
| `change` / `release` | Change files, per-crate bump inference, changelogs, release PRs, tags, forge releases, and publish flow |
| `split` / `sync` | Copybara-style crate extraction and bidirectional sync without a separate DSL |
| `config`, `hash`, `graph` | Config validation, portable planner identities, and explainability tools |

## Release Workflow

Use `change` to review release intent with the code change. Use `release` to calculate versions, update manifests and changelogs, create the release PR, tag the merged result, create forge releases, and publish crates in dependency order.

```bash
cargo rail change add my-crate --bump minor --message "Added graph-aware release planning."
cargo rail change status
cargo rail change check --merge-base --required
cargo rail release run --all --bump auto --pr --check
```

Example check output:

```text
📦 Release Plan

1. my-crate
   Version: 0.1.0 → 0.2.0
   Bump: 0.1.0 -> 0.2.0 (auto: conventional commits -> minor)
   Tag: v0.2.0
   Publish: ✓
   Causes: f956ff8 (minor)

Summary: 1 crate(s), 1 to publish, 1 tag(s), 0 skipped

Changes detected. Run without --check to apply.
```

Apply the reviewed release PR path:

```bash
cargo rail release run --all --bump auto --pr --yes

# after the release PR is merged, from the updated main branch:
cargo rail release finalize --all --yes

# after an interrupted run/finalize, use the state path it printed:
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

Change files live in `.changes/*.md` by default:

```markdown
---
"my-crate" = "minor"
---

Added graph-aware release planning.
```

`--bump auto` reads change files first, then falls back to conventional commits. In workspaces, cargo-rail maps commits to crates and propagates dependency-driven releases through the graph. A shared file can affect several crates without duplicating path-glob rules in the release configuration.
Use `cargo rail change check --merge-base --required` in pre-commit or CI when every changed crate should carry a reviewed change file. It exits `1` when coverage is missing.

## CI Workflow

Use `plan` to decide which surfaces and packages a change affects. Use `run` to execute that scope, or pass `scope.cargo_args` to existing Cargo steps. This lets a repository adopt the planner without replacing its build and test commands.

```bash
cargo rail plan --merge-base
cargo rail plan --merge-base -f github
cargo rail run --merge-base --profile ci
```

Test argument domains are explicit. Use a portable filter, choose a backend for backend-specific options, and place only
test-binary arguments after `--`:

```bash
cargo rail run --surface test --test-filter parser -- --nocapture
cargo rail run --surface test --test-runner cargo --cargo-test-arg=--all-features
cargo rail run --surface test --test-runner nextest --nextest-arg=-P --nextest-arg=commit
```

In GitHub Actions, use [cargo-rail-action](https://github.com/loadingalias/cargo-rail-action) to expose planner scope:

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      build: ${{ steps.rail.outputs.build }}
      test: ${{ steps.rail.outputs.test }}
      docs: ${{ steps.rail.outputs.docs }}
      infra: ${{ steps.rail.outputs.infra }}
      cargo_args: ${{ steps.rail.outputs.cargo-args }}
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - uses: loadingalias/cargo-rail-action@v5
        id: rail
        with:
          version: 0.16.0

  ci:
    needs: [plan]
    if: needs.plan.outputs.build == 'true' || needs.plan.outputs.test == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - name: Build selected crates
        if: needs.plan.outputs.build == 'true'
        env:
          CARGO_ARGS: ${{ needs.plan.outputs.cargo_args }}
        run: cargo build $CARGO_ARGS
      - name: Test selected crates
        if: needs.plan.outputs.test == 'true'
        env:
          CARGO_ARGS: ${{ needs.plan.outputs.cargo_args }}
        run: cargo test $CARGO_ARGS
```

`@v5` is the action interface for cargo-rail v0.16; `version` selects the cargo-rail binary. Pin the action to a full commit SHA when
your supply-chain policy requires immutable actions.

`impact` is diagnostic. `scope` is the execution handoff. In machine output, `scope.cargo_args` is the Cargo package selection (`--workspace`, `-p crate ...`, or empty) so CI does not have to infer it from `scope.mode`.

`cargo rail plan --schema` prints the checked-in v3 JSON Schema. `cargo rail hash` computes a checkout-independent identity for comparing plans; it is deliberately not a build cache key. See the [Planner Machine Contract](docs/planner-contract.md) for compatibility and identity rules.

## Dependency Graph

Use `unify` to inspect or rewrite dependency declarations across workspace manifests.

```bash
cargo rail unify --check
cargo rail unify --check --explain
cargo rail unify
```

It can move shared dependency declarations into `[workspace.dependencies]`, prune dead features, detect features borrowed from another workspace member, detect unused dependencies, derive MSRV, and replace workspace-hack patterns with explicit workspace dependencies. `--check` reports the diff and exits without changing manifests; the command without `--check` applies it.

## Split / Sync

Use `split` and `sync` when a crate needs to live in both a monorepo and a standalone repository.

```bash
cargo rail split init crates/my-crate
cargo rail split run crates/my-crate --check
cargo rail split run crates/my-crate
cargo rail sync crates/my-crate --to-remote
# If manual resolution is required:
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

`split` filters the crate's git history, moves its manifest to the standalone repository layout, and rewrites workspace-relative paths. `sync --to-remote` sends later monorepo commits to that repository. `sync --from-remote` brings standalone changes back on a review branch and writes a receipt when conflicts need manual resolution.

## Replacing existing tooling

| Current setup | cargo-rail path |
| --- | --- |
| `cargo-hakari` / workspace-hack crate | `cargo rail unify` with `pin_transitives = true` |
| `release-plz`, `cargo-release`, `git-cliff` | `cargo rail change` + `cargo rail release run --bump auto`; release intent moves into reviewed change files |
| GitHub path filters + shell scripts | `cargo rail plan` + `cargo rail run` |
| Copybara / subtree scripts | `cargo rail split` + `cargo rail sync` |

## Use another tool when

- A single crate only needs `cargo test` and an occasional tag. Cargo plus a small release tool has less configuration.
- Bazel, Buck2, or Moon already owns the repository's cross-language dependency graph. cargo-rail only models Cargo packages.
- Commit inference is the desired source of release intent. Change files add a review step by design.

## Configuration and references

```bash
cargo rail init
cargo rail config sync
cargo rail config validate
```

Primary references:

- [Configuration Reference](docs/config.md)
- [Command Reference](docs/commands.md)
- [Change Detection Guide](docs/change-detection.md)
- [Planner Machine Contract](docs/planner-contract.md)
- [Architecture](docs/architecture.md)
- [Migrate from cargo-hakari](docs/migrate-hakari.md)
- [Migrate from git-cliff or release-plz](docs/migrate-git-cliff.md)

## Project

- Issues: [GitHub Issues](https://github.com/loadingalias/cargo-rail/issues)
- Crate: [crates.io/cargo-rail](https://crates.io/crates/cargo-rail)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)
- Security: [SECURITY.md](SECURITY.md)
- License: [MIT](LICENSE)
