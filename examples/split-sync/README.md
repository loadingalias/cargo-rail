# Split / Sync Example

Use `split` to extract a crate and `sync` to keep the monorepo and standalone repo aligned.

## Quick Start

```bash
cargo rail split init my-crate
cargo rail split run my-crate --check
cargo rail sync my-crate --to-remote
```

## Reference

- [Configuration Reference](../../docs/config.md)
- [Architecture](../../docs/architecture.md)
