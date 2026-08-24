# Cargo-Rail

**Cargo already knows your workspace. Keep it authoritative.**

Cargo-Rail is the Cargo-native engine for Rust monorepos. It turns Cargo's resolved workspace and exact source state
into affected CI scope, dependency-coherence repairs, Rust visibility analysis, verified compiler reuse, exact-SHA
releases, and Cargo-aware crate synchronization.

**Cargo-Rail does not replace Cargo, nextest, Just, or CI. It gives them one trustworthy scope and removes the shadow
workspace models around them.**

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## One workspace model

Rust monorepos tend to accumulate one partial workspace model per problem: path filters, package-selection scripts,
dependency linters, cache glue, release bots, and repository-sync scripts. Those models drift because they do not share
Cargo's resolved graph or the same source authority.

Cargo-Rail consolidates the decisions without replacing the tools that execute them:

| Decision | Command | Result |
| --- | --- | --- |
| What changed, and what does it affect? | `cargo rail plan` | Typed CI gates and exact Cargo argument arrays |
| Is the dependency graph coherent? | `cargo rail unify` | One reviewable, reversible repair plan |
| Which Rust declarations are dead or too visible? | `cargo rail surface` | Compiler-derived findings and exact visibility fixes |
| Can compiler work be removed safely? | `cargo rail cache` | Verified reuse after `cargo clean`; bounded miss execution |
| What should be released? | `cargo rail change` and `cargo rail release` | Reviewed intent carried through an exact SHA |
| How does a split crate stay tied to the monorepo? | `cargo rail split` and `cargo rail sync` | Cargo-aware history and two-way sync |

**No required SaaS. No second build language. No hand-maintained crate map.**

## Install

Install the general CLI from crates.io:

```bash
cargo install cargo-rail --locked
```

This supports the general CLI, compiler reuse, and `surface --schema`. Surface analysis also needs the matched
compiler-fact driver from a native release archive. On GNU Linux and macOS, install both with the checksum-verifying
installer:

```bash
version=0.22.1
curl --proto '=https' --tlsv1.2 -sSf \
  "https://raw.githubusercontent.com/loadingalias/cargo-rail/v${version}/scripts/install.sh" \
  | sh -s -- "$version"
```

Windows users can install the matching MSVC ZIP from
[GitHub Releases](https://github.com/loadingalias/cargo-rail/releases) and keep its executables together. Musl archives
do not include the surface driver.

## Prove it before you migrate

Inspect a real branch without changing tracked files:

```bash
cargo rail plan --merge-base --explain
cargo rail unify --check --explain
cargo rail cache setup --check
```

- `plan` explains affected work but never executes it.
- `unify --check` reports pending manifest repairs without applying them.
- `cache setup --check` previews the exact machine-owned setup or repair.

Export the same planner decision as a versioned machine contract:

```bash
cargo rail plan --merge-base -f json -o plan.json
cargo rail plan --schema
```

Each package-scoped surface contains the exact `cargo_args` array for Cargo, nextest, Just, or an existing CI job.
Cargo-Rail decides scope; the selected tool executes it. The
[`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) exposes the same gates and scope in GitHub
Actions.

## Affected CI is a graph query

A path filter can tell you which file changed. It cannot reliably tell you what the change affects.

```text
changed source
  → semantic manifest and lockfile analysis
  → Cargo package ownership
  → active reverse-dependency impact
  → build / test / bench / docs / infra / custom surfaces
  → surface-specific package scope
  → typed Cargo arguments and CI gates
```

A formatting-only manifest edit can select no package work. A shared-library change can select its affected dependent
closure. Infrastructure can gate repository work without pretending it belongs to a crate. Incomplete evidence widens
the scope instead of guessing narrowly.

Human explanations, JSON, GitHub output, CI gates, and Cargo arguments all come from the same plan. See
[Planning](docs/planning.md) for the contract and execution handoff.

## Adopt one proof boundary at a time

Every workflow is independent:

1. Observe affected scope with `cargo rail plan --merge-base --explain`.
2. Pass the planner's Cargo arguments into existing jobs.
3. Enforce check-only dependency, surface, and release-intent policy.
4. Enable verified compiler reuse where cold or ephemeral builds justify it.
5. Apply graph repairs, releases, or synchronization only after the team trusts their plans and recovery boundaries.

**Adopt a workflow when it deletes a second source of truth. Do not adopt it merely because Cargo-Rail implements it.**

## Correctness is the product

Cargo-Rail treats fast paths as conditional and side effects as transactions:

- Incomplete planning evidence selects more work.
- Unsupported compiler work bypasses reuse and runs through normal Cargo.
- Snapshot-bound mutations revalidate drift and write only authorized paths.
- Release and synchronization workflows record durable recovery state before irreversible effects.
- Manifest and configuration edits preserve unknown data outside the operation's authority.

Fast paths are used only when their proof boundary holds. See [Architecture](docs/architecture.md) for the ownership and
transaction model.

## Documentation

Start with [Planning](docs/planning.md), then use the [command reference](docs/commands.md) and
[configuration reference](docs/config.md). The detailed cache contract lives in [Caching](docs/caching.md). For failure
recovery, see [Troubleshooting](docs/troubleshooting.md). Runnable configurations live under [Examples](examples/).

## Project

Cargo-Rail is licensed under [MIT](LICENSE). See [Contributing](CONTRIBUTING.md), the
[security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the
[issue tracker](https://github.com/loadingalias/cargo-rail/issues).
