# Architecture

`cargo-rail` is a CLI-first monorepo tool for Rust development. The architecture is simple on purpose.

## Core Rule

Build `WorkspaceContext` once and pass it by reference everywhere else.

That context owns:

- git state
- cargo metadata state (resolved)
- workspace dependency graph
- loaded config

Commands do not reload metadata independently.

## Main Layers

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

## Data Flow

1. Parse CLI
2. Initialize output mode
3. Handle early commands that do not need full context
4. Build `WorkspaceContext`
5. Dispatch the selected command

## Planner Pipeline

`cargo rail plan` follows a fixed path:

1. collect changed files
2. classify file kinds and custom surfaces
3. resolve file ownership
4. compute graph impact
5. emit trace and surface decisions
6. project execution scope

`impact` is diagnostic. `scope` is the execution handoff.
The scope carries the selected mode, selected crates, and ready-to-pass
`cargo_args` so runners do not reconstruct Cargo invocation state.

## Unify Pipeline

`cargo rail unify`:

1. loads cargo metadata
2. analyzes resolved dependencies
3. computes a mutation plan
4. applies lossless TOML edits

## Mutation Authority

Mutation plans are executable contracts, not summaries. Contract v2 binds `HEAD`,
the complete dirty-path snapshot, declared read-only inputs, structured action
payloads, and each authorized file mutation. Apply rejects drift immediately before
the first write. Release commits stage only currently changed authorized paths;
sync commits stage only the paths owned by the source commit.

Split and sync parameters carry validated path capabilities. Source workspace,
Git worktree, crate, target, and temporary roots are canonicalized before use;
overlapping repositories and symlink escapes are rejected before mutation and
revalidated at each filesystem destination.

## Design Choices

- system git instead of libgit2
- petgraph directly instead of another abstraction layer
- lossless TOML editing so manifests keep comments and formatting
- thin `main.rs`, with logic in the library crate

## Where to Change Things

| Change | File or module |
|---|---|
| CLI shape | `src/commands/cli.rs` |
| command dispatch | `src/commands/mod.rs` |
| context loading | `src/workspace/context.rs` |
| planner behavior | `src/commands/plan.rs` |
| graph algorithms | `src/graph/` |
| unify behavior | `src/cargo/` |
| config parsing | `src/config/` |
