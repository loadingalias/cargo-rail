# cargo-rail

> Cargo-native control plane for serious Rust workspaces: graph-aware CI, dependency unification, changesets-style releases, and split/sync.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail) [![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml) [![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## Why This Exists

Rust workspaces usually start with Cargo and end up with path filters, release scripts, changelog glue, subtree jobs, and dependency cleanup tools that do not share one model.

`cargo-rail` uses the two sources of truth Rust teams already have: Cargo metadata and git history.

| Usual workflow | With cargo-rail |
| --- | --- |
| CI path globs guess what changed | `plan` maps files to crates and expands through the dependency graph |
| Release notes are inferred from commits at the end | `change` stores reviewed release intent before release day |
| Multi-crate bumps, changelogs, tags, and publish order drift into scripts | `release` plans them from the workspace graph |
| Workspace dependencies and features drift silently | `unify` catches version, feature, MSRV, and unused-dependency problems |
| Crate extraction means bespoke subtree/Copybara glue | `split` and `sync` keep split repos tied to the canonical workspace |

The bet is simple: Rust workspace automation should be driven by Cargo's resolved graph, not by shell folklore.

## Quick Start

```bash
cargo install cargo-rail

cargo rail init
cargo rail unify --check
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci
```

Pre-built binaries are published with each [GitHub Release](https://github.com/loadingalias/cargo-rail/releases). The
[release target manifest](https://github.com/loadingalias/cargo-rail/blob/main/distribution/release-targets.json) is the
source of truth for supported archives. Every archive is smoke-tested on its native architecture and published with
SHA-256 checksums and signed build provenance.

## What It Covers

| Command | Purpose |
| --- | --- |
| `plan` / `run` | Selective build, test, bench, docs, and infra execution from one deterministic plan |
| `unify` | Workspace dependency unification, feature cleanup, unused dependency detection, and MSRV derivation |
| `change` / `release` | Change files, per-crate bump inference, changelogs, release PRs, tags, forge releases, and publish flow |
| `split` / `sync` | Copybara-style crate extraction and bidirectional sync without a separate DSL |
| `config`, `hash`, `graph` | Config validation, portable planner identities, and explainability tools |

## Release Workflow

Use `change` for explicit release intent and `release` for checks, version bumps, changelogs, release PRs, tags, forge releases, and publishing.

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

`--bump auto` reads change files first, then falls back to conventional commits. In workspaces, cargo-rail attributes changes through the crate graph instead of treating changelogs as path globs plus commit messages.
Use `cargo rail change check --merge-base --required` in pre-commit or CI when every changed crate should carry a reviewed change file. It exits `1` when coverage is missing.

## CI Workflow

Use `plan` to build the deterministic contract and `run` to execute only selected work.

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
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: loadingalias/cargo-rail-action@v4
        id: rail

  ci:
    needs: [plan]
    if: needs.plan.outputs.build == 'true' || needs.plan.outputs.test == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - run: cargo rail run --merge-base --profile ci
```

`impact` is diagnostic. `scope` is the execution handoff. In machine output, `scope.cargo_args` is the Cargo package selection (`--workspace`, `-p crate ...`, or empty) so CI does not have to infer it from `scope.mode`.

`cargo rail plan --schema` prints the checked-in v3 JSON Schema. `cargo rail hash` computes a checkout-independent identity for comparing plans; it is deliberately not a build cache key. See the [Planner Machine Contract](docs/planner-contract.md) for compatibility and identity rules.

## Dependency Graph

Use `unify` to keep the workspace dependency graph lean and honest.

```bash
cargo rail unify --check
cargo rail unify --check --explain
cargo rail unify
```

It can unify workspace dependency declarations, prune dead features, detect undeclared feature borrowing, detect unused dependencies, derive MSRV, and replace workspace-hack patterns with explicit workspace dependencies.

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

## Migration Matrix

| Current setup | cargo-rail path |
| --- | --- |
| `cargo-hakari` / workspace-hack crate | `cargo rail unify` with `pin_transitives = true` |
| `release-plz`, `cargo-release`, `git-cliff` | `cargo rail change` + `cargo rail release run --bump auto` |
| GitHub path filters + shell scripts | `cargo rail plan` + `cargo rail run` |
| Copybara / subtree scripts | `cargo rail split` + `cargo rail sync` |

## Not For You If

- You have a single crate, simple CI, and release once in a while. Use smaller tools.
- You already run Bazel, Buck2, or Moon and want a language-agnostic build graph.
- You want release automation to infer everything from commits with no reviewed release intent.

## Config And Docs

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
