# Architecture

Cargo-Rail is a library-backed Rust workspace engine. It captures one workspace view, makes bounded decisions from
that view, and leaves execution to Cargo, nextest, Just, scripts, CI, Git, forges, and registries.

## One model, separate workflows

| Workflow | Owns | Does not own |
|---|---|---|
| `plan` | Changed inputs, Cargo impact, named-work decisions, typed selectors | Task execution |
| `unify` | Dependency diagnostics and manifest mutation plans | General Cargo execution |
| `surface` | Compiler-derived reachability and visibility plans | Source lints outside its compiler contract |
| cache | Verified compiler facts and results | Cargo freshness or incremental compilation |
| `change` / `release` | Reviewed release intent and durable publication state | Forge, registry, or Git implementation |
| `split` / `sync` | Crate ownership, history mapping, and conflict receipts | General repository synchronization |

These workflows share captured infrastructure but do not invoke one another implicitly.

## Captured workspace authority

`WorkspaceContext` owns the source capture, effective configuration, Cargo metadata, dependency graph, lockfile,
toolchain, targets, and repository boundary used by a command. Derived feature and target views stay bound to those
inputs.

Snapshot-bound commands revalidate the relevant live state before writing. A command may cross a named live boundary,
such as Git readiness or registry state, only when it revalidates that boundary at the operation that uses it.

## Decisions and execution

- Planning emits one typed decision per work item. Consumers lower its package, target, or variant scope directly.
- Compiler facts, native cache results, local CAS objects, and remote objects retain separate identities and
  validation rules.
- Surface accepts only authenticated, complete compiler facts for the selected compiler-crate views.

Incomplete evidence widens or bypasses only its owning decision. It never becomes permission to skip work or restore a
result.

## Mutation boundary

Every filesystem mutation follows the same sequence:

1. Capture the relevant source and repository state.
2. Build a deterministic plan with exact authorized paths.
3. Revalidate the captured assumptions immediately before writing.
4. Apply only the planned changes and persist recovery evidence when needed.

Release, split, and sync persist transaction identity before remote or irreversible effects. External publication is
reconciled through durable state; it is not described as rollback-capable.

## Process and platform boundaries

`src/main.rs` owns process entry, pre-Clap compiler-role dispatch, one context build, diagnostics, and library
dispatch. User-visible behavior belongs in the library.

Compiler roles preserve Cargo's selected program, arguments, wrapper order, working directory, streams, environment,
signals, and exit status. Cache and analysis roles receive narrow inputs and never build a workspace context inside a
compiler process.

`src/windows_fs.rs` is the only production `unsafe` and Win32 FFI boundary. Its safe API remains crate-private.

## Module ownership

| Modules | Responsibility |
|---|---|
| `workspace/`, `source.rs` | Captured workspace authority and derived views |
| `cargo/`, `graph/`, `toml/` | Cargo resolution, graph operations, and lossless TOML edits |
| `planning/` | Typed changes, evidence, named work, impact, and selectors |
| `commands/plan.rs` | Comparison validation and plan rendering |
| `surface.rs`, `commands/surface.rs` | Reachability, policy, findings, mutation plans, and reports |
| `compiler/` | Compiler invocation, facts, sessions, and native-result decisions |
| `cache/`, `remote_cache/` | Local immutable objects, result retention, and remote transport |
| `mutation/` | Drift checks, authorized writes, backups, and receipts |
| `release/`, `split/`, `sync/` | Durable Git, forge, registry, and cross-repository workflows |
| `git/`, path types, process helpers | External capability and containment boundaries |
