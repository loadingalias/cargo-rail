# Real-World Examples

cargo-rail has been tested against several large Rust monorepos to validate
its behavior and performance on non-trivial codebases.

Each subdirectory contains:

- The repository tested
- The exact commands run
- A short demo recording
- A machine-readable summary of the impact

## Examples

| Project | Focus | Key Result |
|---------|-------|------------|
| [tokio](./tokio/) | Async runtime | 10 deps unified, 35 edits saved |
| [polars](./polars/) | DataFrame library | Already well-maintained (2 deps) |
| [ruff](./ruff/) | Python linter | 47% CI reduction with `affected` |
| [vello](./vello/) | GPU 2D rendering | 7 deps unified (virtual workspace) |
| [helix](./helix/) | Text editor | 62% CI reduction, 16 deps unified |
| [tikv](./tikv/) | Distributed KV store | 56 deps unified, 514 edits saved |
| [iced](./iced/) | GUI framework | 164 transitives pinned |
| [meilisearch](./meilisearch/) | Search engine | 46 deps unified + 47% CI reduction |
| [ripgrep](./ripgrep/) | Search tool | 9 deps unified (10 crates, not 1!) |

## Quick Start

```bash
# Clone any example repo
git clone https://github.com/tokio-rs/tokio
cd tokio

# Run cargo-rail
cargo rail init
cargo rail unify --check
cargo rail affected --since HEAD~5
```

## Note

No third-party source code is vendored here. Only CLI transcripts, metrics,
and demo recordings produced by running cargo-rail against public repositories.

Results may vary as upstream repositories evolve. Each example includes the
commit hash used for testing.
