# Cargo-Rail

Cargo-Rail turns Cargo's resolved workspace and exact source state into affected CI scope, dependency repairs, Rust
visibility analysis, verified compiler reuse, exact-SHA releases, and Cargo-aware crate synchronization.

It does not replace Cargo, nextest, Just, or CI. It gives those tools one definitive workspace scope.

[![Crates.io](https://img.shields.io/crates/v/cargo-rail.svg)](https://crates.io/crates/cargo-rail)
[![CI](https://img.shields.io/github/actions/workflow/status/loadingalias/cargo-rail/commit.yaml?branch=main)](https://github.com/loadingalias/cargo-rail/actions/workflows/commit.yaml)
[![MSRV](https://img.shields.io/crates/msrv/cargo-rail)](https://github.com/loadingalias/cargo-rail/blob/main/Cargo.toml)

## What it does

Rust monorepos often maintain separate path filters, package-selection scripts, dependency linters, cache glue,
release bots, and repo-split/sync scripts. These models drift because they don't share Cargo's resolved graph or one
source snapshot. Cargo-Rail derives their decisions from the same workspace view:

| Decision                                          | Command                                      | Result                                                     |
| ------------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------- |
| What changed, and what does it affect?            | `cargo rail plan`                            | Typed CI gates and exact Cargo argument arrays             |
| Is the dependency graph coherent?                 | `cargo rail unify`                           | One reviewable, reversible repair plan                     |
| Which Rust declarations are dead or too visible?  | `cargo rail surface`                         | Compiler-derived findings and exact visibility fixes       |
| Can compiler work be removed safely?              | `cargo rail cache`                           | Verified reuse after `cargo clean`; bounded miss execution |
| What should be released?                          | `cargo rail change` and `cargo rail release` | Reviewed intent carried through an exact SHA               |
| How does a split crate stay tied to the monorepo? | `cargo rail split` and `cargo rail sync`     | Cargo-aware history and two-way sync                       |

## Installation

Install complete Cargo-Rail, including Surface analysis and compiler-cache helpers.

Apple Silicon macOS and Linux:

```bash
curl --proto '=https' --tlsv1.2 -fsSL \
  https://github.com/loadingalias/cargo-rail/releases/latest/download/cargo-rail-installer.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/loadingalias/cargo-rail/releases/latest/download/cargo-rail-installer.ps1 | iex
```

The installer selects the native archive, verifies its SHA-256 digest, and places every authenticated component in
Cargo's binary directory. Run `cargo rail surface --prepare` to authenticate and prepare the producer for the
workspace's exact Cargo-selected toolchain before analysis. When the required compiler library is absent, the preflight
installs that toolchain's `rustc-dev` component through `rustup`; it never changes the user default. Surface analysis is
unavailable only in the musl archive.

For a core-only source build, use `cargo install cargo-rail --locked` or `cargo binstall cargo-rail`. These builds keep
all commands except Surface analysis; `surface --schema` still works. This is an alternative install path, not a
prerequisite for the complete installer. The complete path requires `rustup`.

## Try it without changing files

Inspect a real branch without changing tracked files:

```bash
cargo rail plan --merge-base --explain
cargo rail unify --check --explain
cargo rail cache setup --check
cargo rail surface --prepare
```

`plan` explains affected work without executing it. `unify --check` reports pending manifest repairs.
`cache setup --check` previews machine-owned setup or repair.

Export the same planner decision as a versioned machine contract:

```bash
cargo rail plan --merge-base -f json -o plan.json
cargo rail plan --schema
```

Each package-scoped surface contains the exact `cargo_args` array for Cargo, nextest, Just, or CI. Cargo-Rail decides
scope; the selected tool executes it. The [`cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action)
exposes the same gates and scope in GitHub Actions.

## Affected CI is a graph query

A path filter finds changed files. Cargo-Rail also resolves ownership and reverse-deps impact:

```text
changed source
  → semantic manifest and lockfile analysis
  → Cargo package ownership
  → active reverse-dependency impact
  → build / test / bench / docs / infra / custom surfaces
  → surface-specific package scope
  → typed Cargo arguments and CI gates
```

A formatting-only manifest edit can select no package work. A shared-lib change can select its dependent closure.
Infra can gate repo work without belonging to a crate. Incomplete evidence widens scope.

Human explanations, JSON, GitHub output, CI gates, and Cargo arguments all come from the same plan. See
[Planning](docs/planning.md) for the contract and execution handoff.

## Adopt one workflow at a time

Every workflow is independent:

1. Observe affected scope with `cargo rail plan --merge-base --explain`.
2. Pass the planner's Cargo arguments into existing jobs.
3. Enforce check-only dependency, surface, and release-intent policy.
4. Enable verified compiler reuse where cold or ephemeral builds justify it.
5. Apply graph repairs, releases, or synchronization after reviewing their plans and recovery paths.

## Safety boundaries

Cargo-Rail treats fast paths as conditional and side effects as transactions:

- Incomplete planning evidence selects more work.
- Unsupported compiler work bypasses reuse and runs through normal Cargo.
- Snapshot-bound mutations revalidate drift and write only authorized paths.
- Release and synchronization workflows record durable recovery state before irreversible effects.
- Manifest and configuration edits preserve unknown data outside the operation's authority.

See [Architecture](docs/architecture.md) for the ownership and transaction model.

## Documentation

Start with [Planning](docs/planning.md). Use the [command reference](docs/commands.md),
[configuration reference](docs/config.md), [cache contract](docs/caching.md), and
[troubleshooting guide](docs/troubleshooting.md) as needed. Focused configurations live under [Examples](examples/).

## Project

Cargo-Rail is licensed under [MIT](LICENSE). See [Contributing](CONTRIBUTING.md), the
[security policy](SECURITY.md), [releases](https://github.com/loadingalias/cargo-rail/releases), and the
[issue tracker](https://github.com/loadingalias/cargo-rail/issues).
