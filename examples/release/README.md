# Release Example

Use `release` for checks, changelog generation, tags, and publish ordering.

## Quick Start

```bash
cargo rail release check my-crate
cargo rail release run my-crate --bump patch --check
cargo rail release run my-crate --bump patch --yes
git push origin main --follow-tags
```

## Reference

- [Configuration Reference](../../docs/config.md)
- [Architecture](../../docs/architecture.md)
