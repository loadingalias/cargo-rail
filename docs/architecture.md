# Architecture

Cargo-Rail is a library-backed Rust workspace engine.
It captures one workspace view, makes bounded decisions from that view,
and coordinates the operations it owns.
Cargo, nextest, Just, scripts, CI, Git, forges,
and registries remain the executors of their own operations.

## One model, separate workflows

| Workflow | Owns | Does not own |
| --- | --- | --- |
| `plan` | Changed inputs, Cargo impact, named-work decisions, typed selectors | Task execution |
| `unify` | Dependency diagnostics and manifest mutation plans | General Cargo execution |
| `surface` | Compiler-derived reachability and visibility plans | Source lints outside its compiler contract |
| cache | Verified compiler facts and results | Cargo freshness or incremental compilation |
| `change` / `release` | Reviewed release intent and durable publication state | Forge, registry, or Git implementation |
| `split` / `sync` | Crate ownership, history mapping, and conflict receipts | General repository synchronization |

These workflows share captured infrastructure but do not invoke one another implicitly.

## Captured workspace authority

`WorkspaceContext` owns the source capture, effective configuration, Cargo metadata, dependency graph, lockfile,
toolchain, targets, and repository boundary used by a command.
Derived feature and target views stay bound to those inputs.

Snapshot-bound commands revalidate the relevant live state before writing.
A command may cross a named live boundary, such as Git readiness or registry state,
only when it revalidates that boundary at the operation that uses it.

## Decisions and execution

- Planning emits one typed decision per work item.
  Consumers lower its package, target, or variant scope directly.
- Compiler facts, native cache results, local CAS objects,
  and remote objects retain separate identities and validation rules.
- Surface accepts only authenticated, complete compiler facts for the selected compiler-crate views.

Incomplete evidence widens or bypasses only its owning decision.
It never becomes permission to skip work or restore a result.

Compiler-backed Unify and Surface work uses one acquisition engine:

1. Build a deterministic target and feature schedule from the captured workspace.
1. Admit only identity-matched, complete cached evidence.
1. Run remaining views with bounded process slots, work permits, sandboxes, output,
   and artifact storage.
1. Persist resumable progress before dispatch and cancel complete process trees after a failure.
1. Integrate completed work in deterministic order, independent of worker completion order.

## Mutation boundary

Planned workspace edits, including Unify and Surface changes, follow this sequence:

1. Capture the relevant source and repository state.
1. Build a deterministic plan with exact authorized paths.
1. Revalidate the captured assumptions immediately before writing.
1. Apply only the planned changes and persist recovery evidence when needed.

Cache writes, compiler acquisition journals,
and installation profiles have their own validation and recovery boundaries;
they do not pass through a workspace mutation plan.
Release, split, and sync persist transaction identity before remote or irreversible effects.
Recovery reconciles external publication; it cannot undo publication.

## Process and platform boundaries

`src/main.rs` owns process entry, pre-Clap compiler-role dispatch, one context build, diagnostics,
and library dispatch.
User-visible behavior belongs in the library.

Compiler wrappers preserve the selected compiler and its execution contract
when reuse is unavailable.
Eligible cache operations may run through the authenticated compiler-matched driver
or restore validated outputs.
Cache and analysis roles receive bounded invocation inputs
and do not build a workspace context inside a compiler process.

`src/windows_fs.rs` is the only production `unsafe` and Win32 FFI boundary.
Its safe API remains crate-private.

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
