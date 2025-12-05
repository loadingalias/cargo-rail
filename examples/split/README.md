# split/sync demo

Demonstrates crate extraction with git history preservation using ruff's `ruff_cache` crate.

## Workflow

```bash
cargo rail init                        # Base config
cargo rail split init ruff_cache       # Configure crate for split
cargo rail split run ruff_cache --check  # Preview
cargo rail split run ruff_cache        # Execute split
cargo rail sync ruff_cache --check     # Preview sync
```

## Config

```toml
[crates.ruff_cache.split]
remote = "/tmp/ruff-split-test"
branch = "main"
mode = "single"
paths = [{ crate = "crates/ruff_cache" }]
include = ["LICENSE"]
```

| Option | Value | Why |
|--------|-------|-----|
| `remote` | `/tmp/...` | Local path for demo (use git URL in production) |
| `mode` | `single` | One crate per split repo |
| `paths` | `[{ crate = "..." }]` | Crate location in monorepo |
| `include` | `["LICENSE"]` | Extra files to copy |

## What this shows

- `cargo rail split init` - Configure a crate for splitting
- `cargo rail split run --check` - Preview before executing
- `cargo rail split run` - Extract crate with full git history
- `cargo rail sync --to-remote` - Push monorepo changes to split
- `cargo rail sync --from-remote` - Pull split repo changes back

## Demo

https://github.com/user-attachments/assets/b9f56e77-de0a-42c1-b2ef-1a40bb24f5ac
