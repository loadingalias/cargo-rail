# Architecture

Cargo-Rail is one library-backed workspace engine around Cargo's resolved model. Commands share captured source,
configuration, toolchain, metadata, and graph state instead of rebuilding partial views of the workspace.

## Mental model

Cargo-Rail has five workflows:

| Workflow             | Authority                                                | Result                                                         |
| -------------------- | -------------------------------------------------------- | -------------------------------------------------------------- |
| `unify`              | Resolved Cargo graph plus compiler evidence              | A checked or applied manifest mutation plan                    |
| `plan`               | Changed source plus the declared dependency universe     | Selected surfaces and typed package scopes                     |
| caching              | Exact compiler inputs and verified result bytes          | Diagnostic reuse or one compiler-result restore                |
| `change` / `release` | Reviewed change intent plus exact Git and registry state | Versions, changelogs, publications, and durable recovery state |
| `split` / `sync`     | Captured source, Git history, and split ownership        | Standalone history or mapped changes with origin evidence      |

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

## Planning and direct consumption

`plan` owns change classification, crate ownership, conservative reverse dependency impact, surface selection, and
package scope. Its declared dependency universe includes optional and target-gated edges without changing the exact
Cargo graph. Cargo, cargo-nextest, Just, and CI consume each surface's final `cargo_args` array directly.

Text explanations, JSON, schema, hashing, and GitHub projections all come from this protocol. See
[Planning](planning.md).

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
The compiler-evidence and native compiler-result layers are documented in [Caching](caching.md).

## Compiler process boundary

Compiler roles are classified once, before Clap and workspace capture. The invocation boundary captures Cargo's exact
program and argv. Transparent execution preserves the live working directory, inherited non-private environment,
wrapper order, streams, signals, and exit status. The analysis role adds only its owned lint or observation-output
arguments. Cache and compiler-fact domains receive narrow invocation inputs; neither constructs a command plan or
`WorkspaceContext` inside a rustc process.

The wrapper order is Cargo-Rail cache, analysis workspace driver, explicitly compatible existing workspace wrapper,
then the selected compiler. The cache and fact domains retain separate identities and authority. Analysis uses a
private capability bound to one source root and observation directory. Shared immutable objects and output manifests
belong to `cache/`; compiler sessions and evidence remain in `compiler/`.

## Module ownership

| Modules                                    | Responsibility                                                                                 |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------- |
| `workspace/`, `source/`                    | Captured authority and derived workspace views                                                 |
| `cargo/`, `graph/`, `toml/`                | Cargo resolution, graph algorithms, and lossless editing                                       |
| `change_detection/`, `commands/plan.rs`    | File semantics, impact, surfaces, and scope                                                    |
| `compiler/`                                | Pre-Clap compiler invocation, sessions, observations, diagnostics, and native-result decisions |
| `cache/`                                   | Shared immutable CAS primitives, retained output manifests, measurement, and reclamation       |
| `mutation/`                                | Plan/apply drift checks, authorized paths, and receipts                                        |
| `release/`, `split/`, `sync/`              | Workflows that cross repository or publication boundaries                                      |
| `git/`, source path types, process helpers | External capabilities and containment                                                          |

`src/main.rs` remains a thin process entry point. User-visible behavior belongs in the library.
