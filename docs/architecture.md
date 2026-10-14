# Architecture

Cargo-Rail is a library-backed Rust workspace engine.
It captures one view of the workspace and makes bounded decisions from that view.
It also coordinates the operations that it owns.
Cargo, nextest, Just, scripts, CI, Git, forges,
and registries stay the executors of their own operations.

## One model, separate workflows

| Workflow | Owns | Does not own |
| --- | --- | --- |
| `plan` | Changed inputs, Cargo impact, named-work decisions, typed selectors | Task execution |
| `unify` | Dependency diagnostics and manifest mutation plans | General Cargo execution |
| `surface` | Compiler-derived reachability and visibility plans | Source lints outside its compiler contract |
| cache | Verified compiler facts and results | Cargo freshness or incremental compilation |
| `change` / `release` | Reviewed release intent and durable publication state | Forge, registry, or Git implementation |
| `split` / `sync` | Crate ownership, history mapping, and conflict receipts | General repository synchronization |

These workflows share captured infrastructure.
A workflow does not implicitly start another workflow.

## Captured workspace authority

`WorkspaceContext` owns the inputs that a command uses: the source capture, effective configuration, Cargo metadata,
dependency graph, lockfile, toolchain, targets, and repository boundary.
Derived feature and target views stay bound to those inputs.

Before a snapshot-bound command writes, it revalidates the applicable live state.
A command can cross a named live boundary only when it revalidates that boundary at the operation
that uses it.
Git readiness and registry state are examples of named live boundaries.

## Decisions and execution

- Planning produces one typed decision for each work item.
  Consumers lower the package, target, or variant scope of that decision directly.
- Compiler facts, native cache results, local CAS objects,
  and remote objects keep separate identities and validation rules.
- Surface accepts only authenticated, complete compiler facts for the selected compiler-crate views.

Incomplete evidence widens or bypasses only the decision that owns it.
Incomplete evidence never gives permission to skip work or restore a result.

Compiler-backed Unify and Surface work uses one acquisition engine:

1. The engine builds a deterministic target and feature schedule from the captured workspace.
1. It admits only cached evidence that is complete and has a matching identity.
1. It runs the remaining views one Cargo process at a time.
   Views of one target and command class reuse one sandbox,
   so Cargo compiles each shared dependency unit once.
   Cargo owns job parallelism inside each view.
   Output and artifact storage stay bounded.
1. It persists resumable progress before dispatch.
   After a failure, it cancels complete process trees.
1. It integrates completed work in deterministic order.
   This order does not depend on the order in which workers complete.

## Mutation boundary

Planned workspace edits, including Unify and Surface changes, follow this sequence:

1. Cargo-Rail captures the applicable source and repository state.
1. Cargo-Rail builds a deterministic plan with exact authorized paths.
1. Immediately before it writes, Cargo-Rail revalidates the captured assumptions.
1. Cargo-Rail applies only the planned changes.
   When necessary, it persists recovery evidence.

Cache writes, compiler acquisition journals,
and installation profiles have their own validation and recovery boundaries.
They do not pass through a workspace mutation plan.
Split and sync persist the transaction identity before a remote or irreversible effect.
A release persists nothing of its own.
Git, the registry, and GitHub hold its progress.
Recovery reconciles external publication.
Recovery cannot undo publication.

## Release authority

`release::engine` derives every release decision from state outside Cargo-Rail:

- The `Rail-Release-Crates` trailer names the crates and versions of a release commit.
- A remote tag names its release commit.
- In the registry, `.cargo_vcs_info.json` names the commit that each package came from.
- GitHub holds drafts, assets, attestations, and pull requests.

A rerun reads this state and completes the missing effects.
No record can direct an effect.
The protected branch is the authority for the release.
A release commit lands on that branch through a leased atomic push or a merged pull request.
The checks on that branch gate the first effect.

The planner (`release::planner`, `cargo_projection`, `change_files`, `changelog`) computes the bytes of the release commit.
The engine packages and publishes from a temporary clone of the tagged commit.

The companion Action installs the locked Cargo-Rail version.
It runs `cargo rail release` and publishes the result.
Repository workflows own runners, credentials, and product packaging.
They do not implement another release planner or publisher.
See [the release guide](releases.md).

## Process and platform boundaries

`src/main.rs` owns process entry, pre-Clap compiler-role dispatch, one context build, diagnostics,
and library dispatch.
User-visible behavior belongs in the library.

When reuse is not available, compiler wrappers keep the selected compiler
and its execution contract.
An eligible cache operation can run through the authenticated driver that matches the compiler.
It can also restore validated outputs.
Cache and analysis roles receive bounded invocation inputs.
They do not build a workspace context inside a compiler process.

`src/windows_fs.rs` is the only production `unsafe` and Win32 FFI boundary.
Its safe API stays crate-private.
The separately built compiler driver has one `unsafe` module, `macro_observation`.
On Linux and macOS, this module instruments procedural-macro images to record what they read.

## Module ownership

| Modules                             | Responsibility |
| ----------------------------------- | -------------- |
| `workspace/`, `source.rs`           | Captured workspace authority and derived views |
| `cargo/`, `graph/`, `toml/`         | Cargo resolution, graph operations, and lossless TOML edits |
| `planning/`                         | Typed changes, evidence, named work, impact, and selectors |
| `commands/plan.rs`                  | Comparison validation and plan rendering |
| `surface.rs`, `commands/surface.rs` | Reachability, policy, findings, mutation plans, and reports |
| `compiler/`                         | Compiler invocation, facts, sessions, and native-result decisions |
| `cache/`                            | Installation profiles, local CAS authority, result retention, and usage reporting |
| `remote_cache/`                     | Machine-owned provider authority and remote transport |
| `mutation/`                         | Drift checks, authorized writes, backups, and receipts |
| `release/`, `split/`, `sync/`       | Durable Git, forge, registry, and cross-repository workflows |
| `git/`, path types, process helpers | External capability and containment boundaries |
