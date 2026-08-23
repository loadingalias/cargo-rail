# Architecture

Cargo-Rail is one library-backed workspace engine around Cargo's resolved model. Commands share captured source,
configuration, toolchain, metadata, and graph state instead of rebuilding partial views of the workspace.

## Mental model

Cargo-Rail has six workflows:

| Workflow             | Authority                                                        | Result                                                         |
| -------------------- | ---------------------------------------------------------------- | -------------------------------------------------------------- |
| `unify`              | Resolved Cargo graph plus compiler evidence                      | A checked or applied manifest mutation plan                    |
| `plan`               | Changed source plus the declared dependency universe             | Selected surfaces and typed package scopes                     |
| `surface`            | Authenticated compiler facts plus exact compiler-crate authority | Reachability findings or exact visibility mutations            |
| caching              | Exact compiler inputs and verified result bytes                  | Diagnostic reuse or one compiler-result restore                |
| `change` / `release` | Reviewed change intent plus exact Git and registry state         | Versions, changelogs, publications, and durable recovery state |
| `split` / `sync`     | Captured source, Git history, and split ownership                | Standalone history or mapped changes with origin evidence      |

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
The compiler-evidence and native compiler-result layers are documented in [Caching](caching.md). `surface` consumes
complete authenticated typed facts from that compiler boundary; it does not reconstruct a second source graph.

## Surface authority and report protocol

Surface authority belongs to compiler crates: package, Cargo target, Rust crate name, target kind, and observation
role. A selected binary product is closed independently of package publishability. With the explicit workspace
consumer-scope assertion, a non-publishable library, proc-macro, or build-script crate is also closed; a publishable
library or configured external crate remains open. When one physical declaration has both open and closed compiler
observations, the open observation wins. Selected internal libraries seed production reachability only from their
actual cross-crate production consumers.

Every inspection, check, and mutation projection uses surface contract v2. It records audited and open compiler
targets, selected products and target selectors, exact feature/target views, completeness, policy levels,
configuration diagnostics, cache observations, acquisition metrics, and the exact mutation plan. Inspection is
read-only and non-failing; `--check` turns configuration errors and deny-level findings into exit 1. Operational
failures exit 2. Machine output is one schema-owned stdout value.

Source-built installations deliberately have no compiler-analysis authority. Schema output remains pre-context, but
analysis rejects the installation before Cargo metadata or workspace acquisition. Supported native release archives
carry the matching driver beside the CLI; the driver protocol verifies binary identity and captured workspace
capability before accepting compiler facts.

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
| `surface.rs`, `commands/surface.rs`        | Rust declaration reachability, diagnostic policy, exact visibility plans, and reports          |
| `compiler/`                                | Pre-Clap compiler invocation, sessions, observations, diagnostics, and native-result decisions |
| `cache/`                                   | Shared immutable CAS primitives, retained output manifests, measurement, and reclamation       |
| `mutation/`                                | Plan/apply drift checks, authorized paths, and receipts                                        |
| `release/`, `split/`, `sync/`              | Workflows that cross repository or publication boundaries                                      |
| `git/`, source path types, process helpers | External capabilities and containment                                                          |

`src/main.rs` remains a thin process entry point. User-visible behavior belongs in the library.
