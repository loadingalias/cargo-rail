# Split / Sync Example

`split` filters crate history and rewrites workspace-relative paths for a standalone repository. `sync` maps later commits in either direction and uses a three-way merge for concurrent edits.

## Quick Start

```bash
cargo rail split init my-crate
cargo rail split run my-crate --check
cargo rail split run my-crate
cargo rail sync my-crate --to-remote
```

`sync --from-remote` creates a review branch in the monorepo. Manual conflicts leave files and a receipt for `cargo rail sync --resume <receipt>`.

## Reference

- [Configuration Reference](../../docs/config.md)
- [Architecture](../../docs/architecture.md)
