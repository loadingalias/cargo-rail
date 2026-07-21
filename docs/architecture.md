# Architecture

`cargo-rail` is a library-backed CLI. Commands share one workspace snapshot and use explicit plans for filesystem, git, and release mutations.

## Workspace snapshot

Build `WorkspaceContext` once and pass it by reference everywhere else.

The context owns:

- git state
- resolved Cargo metadata
- workspace dependency graph
- loaded config

Commands do not reload metadata independently.

## Modules

| Layer | Purpose |
|---|---|
| `commands/` | CLI handlers and command dispatch |
| `action` | shell-free action declarations and deterministic request expansion |
| `workspace/` | `WorkspaceContext` construction and shared state |
| `graph/` | dependency graph queries |
| `cargo/` | metadata, manifests, unify logic |
| `git/` | system git integration |
| `change_detection/` | file classification and planner taxonomy |
| `release/` | release planning and publishing |
| `split/` / `sync/` | repository extraction and bidirectional sync |
| `toml/` | lossless manifest editing |

## Command flow

1. Parse CLI arguments.
2. Initialize text or machine output.
3. Handle commands that do not need workspace metadata.
4. Build `WorkspaceContext` once.
5. Dispatch the command with a shared reference.

## Planner pipeline

`cargo rail plan` follows a fixed path:

1. Collect changed files.
2. Classify built-in and custom surfaces.
3. Resolve crate ownership.
4. Walk reverse dependencies to calculate impact.
5. Record stable reason codes and surface decisions.
6. Project the result into execution scope.

`impact` explains the graph calculation. `scope` carries the selected mode, crates, surfaces, and `cargo_args` used by runners.

`cargo rail run` maps planner surfaces to exact built-in or repository actions before starting the first action
process. Preview, local execution, CI JSON/GitHub plans, and the versioned decision receipt consume the same stable
topological expansion. Only the process boundary resolves and revalidates repository paths and applies typed
environment capabilities. The graph rejects missing snapshot identity, duplicate action IDs, unknown or repeated
dependencies, cycles, output overlap, and path escape before execution. Generated actions have one declared owner and
separate check/regenerate argv. Each expansion carries a versioned action-key analysis over exact declared source,
resolution, target, toolchain, configuration, argv, and environment identity. Incomplete ambient, process, build-script,
proc-macro, external-source, or dependency-result evidence is reported as `uncacheable`; cargo-rail does not issue an
authorizing key for it.

## Unify pipeline

`cargo rail unify`:

1. Load the host metadata once, then resolve configured targets in parallel into one indexed workspace model.
2. Derive the rustc target cfg sets and source-referenced feature selections needed to exercise conditional code.
3. Collect workspace-only compiler evidence, reusing diagnostics only when the compiler, source, manifest, target, and feature-selection identity still match.
4. Analyze versions, features, MSRV, undeclared features, and unused edges from the shared model.
5. Build a deterministic mutation plan with portable proof certificates for graph-removing decisions.
6. Revalidate exact declaration scopes and the resulting Cargo graph before applying lossless TOML edits.

The compiler wrapper passes dependency linting only to workspace compilation units. Registry, git, build-script, and proc-macro units keep Cargo's normal arguments, avoiding failures in third-party code. Open-world packages preserve public feature and optional-dependency surfaces; `consumer_scope = "workspace"` explicitly authorizes closed-world cleanup for non-published packages.

## Compilation observations

Compiler evidence uses a versioned compilation-unit identity, not a package identity. A unit binds its Cargo package,
typed target kind and name, crate types, host/target role, target specification, profile, features, `cfg`, emit modes,
link responsibility, normalized compiler argv, and exact dependency-artifact edges. Libraries, binaries, tests,
examples, benches, documentation, proc macros, build scripts, and native-link responsibility remain distinct even when
the class is not reusable.

During workspace-only `rustc` diagnostics, cargo-rail records argv-declared inputs before execution and correlates the
completed invocation with Cargo's stable JSON artifact messages. Rustc dep-info supplies observed file and environment
reads. The immutable result manifest keeps declared inputs, observed reads, dependency artifacts, emitted outputs, and
execution metadata in separate fields. Every file is SHA-256 digested from its bytes and re-digested before diagnostic
evidence can be reused. Cargo's `fresh` flag is retained only as execution metadata; it never authorizes a hit.

Selected and underlying Cargo, rustc, and rustdoc implementations, wrappers, configured linkers and runners, and
repository executables are content-addressed when relevant. Scripts also bind direct interpreters. Response-file
expansion, dynamic libraries, SDK inputs, default linkers, incomplete platform images, and missing stable rustdoc
invocation evidence produce explicit bypasses. Observation manifests never become pre-execution `ActionKey` inputs,
and cargo-rail neither stores result artifacts nor writes or restores Cargo fingerprint state.

## Mutation authority

Mutation contract v2 binds `HEAD`, the dirty-path snapshot, declared read-only inputs, structured actions, and every authorized file mutation. Apply rechecks that state immediately before the first write. Release commits stage only changed authorized paths. Sync commits stage only paths owned by the source commit.

Split and sync resolve Cargo member names, dependency closure, release boundaries, and explicit non-Cargo assets from
the same `WorkspaceSnapshot` used by planning and release. Versioned `Rail-Origin` trailers bind synthesized commits
to the source repository, source commit, ownership snapshot, and transform version. Ordinary heads, remote refs, and
tags are the mapping database; Git notes are read only during lossless migration.

Source, target, and temporary roots are canonicalized before mutation. Overlapping repositories and symlink escapes
are rejected, and each destination is revalidated before writing. Exact Git trees preserve deletions, modes, symlinks,
parents, and separate author/committer identities without staging unrelated worktree state.

## Dependency choices

- System `git` handles repository operations and three-way merges.
- `petgraph` provides graph storage and traversal without a project-specific graph abstraction.
- `toml_edit` preserves manifest comments and formatting.
- `main.rs` handles startup and error reporting; production behavior remains in the library.

## Code map

| Change | File or module |
|---|---|
| CLI shape | `src/commands/cli.rs` |
| command dispatch | `src/commands/mod.rs` |
| context loading | `src/workspace/context.rs` |
| planner behavior | `src/commands/plan.rs` |
| graph algorithms | `src/graph/` |
| unify behavior | `src/cargo/` |
| config parsing | `src/config/` |
| release planning and state | `src/release/` |
| split and sync mutations | `src/split/`, `src/sync/` |
