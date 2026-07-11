# Config Example

[`rail.toml`](rail.toml) shows every major configuration section in one file. Copy only the sections the workspace needs.

## Validate it

```bash
cargo rail init
cargo rail config validate --strict
```

To use it as a starting point:

```bash
cp examples/config/rail.toml .config/rail.toml
```

## Reference

- [Configuration Reference](../../docs/config.md)
