# Architecture

Cargo-Rail is one library-backed workspace engine around Cargo's resolved model. Commands share captured source,
configuration, toolchain, metadata, and graph state instead of rebuilding partial views of the workspace.

## Mental model

Cargo-Rail has five workflows:

| Workflow | Authority | Result |
|---|---|---|
| `unify` | Resolved Cargo graph plus compiler evidence | A checked or applied manifest mutation plan |
| `plan` / `run` | Changed source plus resolved dependency graph | Selected surfaces, package scopes, and ordered actions |
| caching | Exact action inputs and verified result bytes | Diagnostic reuse, one compiler-result restore, or one whole-action restore |
| `change` / `release` | Reviewed change intent plus exact Git and registry state | Versions, changelogs, publications, and durable recovery state |
| `split` / `sync` | Captured source, Git history, and split ownership | Standalone history or mapped changes with origin evidence |

These workflows share infrastructure, not hidden control flow. Running `plan` does not run `unify`; running `release`
does not silently invoke split or sync.

## One captured workspace view

`main.rs` handles CLI preparation and pre-context commands, then builds one `WorkspaceContext` and passes it to the
selected command. Depending on the command, that context owns:

- the Git worktree and repository boundary;
- Cargo metadata and the base dependency graph;
- parsed `rail.toml`;
- captured source, manifests, lockfile, Cargo configuration, toolchain, and target identities; and
- lazy feature/target resolution views derived from those inputs.

Source capture happens before metadata can create generated state. Snapshot-bound commands revalidate live inputs
before mutation. Commands derive narrower views from the context; they do not reload an independent workspace model
for convenience.

## Planning and execution

`plan` owns change classification, crate ownership, reverse dependency impact, surface selection, and package scope.
`run` consumes that plan, refines Cargo actions for their exact feature and target views, expands direct argv arrays,
validates the complete action graph, and only then starts processes.

Text explanations, JSON and GitHub projections, dry-run previews, decision receipts, and execution all come from this
protocol. See [Planning and execution](planning.md).

## Mutation and external effects

Filesystem mutation follows check, plan, revalidate, apply:

1. Capture the relevant source and Git state.
2. Build deterministic actions with exact authorized paths.
3. Revalidate the captured assumptions immediately before writing.
4. Apply only the planned changes and record a receipt or backup where recovery requires one.

Release, split, and sync add Git, forge, remote, or registry effects. They persist enough identity and progress before
crossing those boundaries to make retry or reconciliation explicit. They do not claim that an external publication can
be rolled back.

Paths are validated capabilities, not unchecked strings. Manifest and configuration edits preserve TOML data outside
the operation's ownership.

## Cache authority

A cache lookup is never proof by itself. Cargo-Rail revalidates the inputs, action/result binding, and stored bytes
owned by that cache layer before reuse. Unsupported or incomplete evidence bypasses reuse and executes the normal tool.
The three cache layers and their current support are documented in [Caching](caching.md).

## Module ownership

| Modules | Responsibility |
|---|---|
| `workspace/`, `source/` | Captured authority and derived workspace views |
| `cargo/`, `graph/`, `toml/` | Cargo resolution, graph algorithms, and lossless editing |
| `change_detection/`, `commands/plan.rs` | File semantics, impact, surfaces, and scope |
| `action.rs`, `action_key.rs`, `commands/run.rs` | Action expansion, validation, identity, and execution |
| `compiler/`, `hermetic/` | Compiler observation and verified reuse boundaries |
| `mutation/` | Plan/apply drift checks, authorized paths, and receipts |
| `release/`, `split/`, `sync/` | Workflows that cross repository or publication boundaries |
| `git/`, source path types, process helpers | External capabilities and containment |

`src/main.rs` remains a thin process entry point. User-visible behavior belongs in the library.
