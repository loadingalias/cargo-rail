# Examples

cargo-rail has been tested against several large, respected Rust monorepos to validate the behavior and performance on non-trivial codebases.

## Feature Workflows

| Workflow | Description |
|----------|-------------|
| [change-detection](./change-detection/) | Graph-aware CI optimization with `affected` and `test` |
| [release](./release/) | Versioning, changelog generation, and publishing |
| [split-sync](./split-sync/) | Crate extraction and bidirectional sync |

## Real-World Monorepos

Each subdirectory contains:

- The repository tested
- The exact commands run
- A short demo recording
- A machine-readable summary of the impact

| Project | Focus | Key Result |
|---------|-------|------------|
| [codex](./codex/) | AI CLI (48 crates) | 2 deps unified, 19 edits saved |
| [helix](./helix/) | Text editor (13 crates) | 16 deps unified, 66 edits saved |
| [helix-db](./helix-db/) | Graph database (6 crates) | 16 deps unified, 44 edits saved |
| [iced](./iced/) | GUI framework (71 crates) | 6 deps unified, 20 edits saved |
| [jj](./jj/) | Git-compatible VCS (5 crates) | Well-maintained, minimal changes |
| [meilisearch](./meilisearch/) | Search engine (19 crates) | 46 deps unified, 209 edits saved |
| [polars](./polars/) | DataFrame library (33 crates) | Well-maintained, 214 dead features pruned |
| [ripgrep](./ripgrep/) | Search tool (10 crates) | 9 deps unified, 35 edits saved |
| [ruff](./ruff/) | Python linter (43 crates) | Well-maintained, minimal changes |
| [tikv](./tikv/) | Distributed KV (83 crates) | 57 deps unified, 516 edits saved |
| [tokio](./tokio/) | Async runtime (10 crates) | 10 deps unified, 35 edits saved |
| [vello](./vello/) | GPU 2D rendering (26 crates) | 7 deps unified, 17 edits saved |

## Note

No third-party source code is vendored here. Only CLI transcripts, metrics,
and demo recordings produced by running cargo-rail against public repositories.

Results may vary as upstream repositories evolve. Each example includes the
commit hash used for testing.
