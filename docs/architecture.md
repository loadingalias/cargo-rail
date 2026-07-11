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

## Unify pipeline

`cargo rail unify`:

1. Load resolved metadata for the configured targets.
2. Analyze versions, features, MSRV, and unused edges.
3. Build a deterministic mutation plan.
4. Revalidate the plan and apply lossless TOML edits.

## Mutation authority

Mutation contract v2 binds `HEAD`, the dirty-path snapshot, declared read-only inputs, structured actions, and every authorized file mutation. Apply rechecks that state immediately before the first write. Release commits stage only changed authorized paths. Sync commits stage only paths owned by the source commit.

Split and sync canonicalize the source workspace, worktree, crate, target, and temporary roots. They reject overlapping repositories and symlink escapes before mutation and revalidate each destination before writing.

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
