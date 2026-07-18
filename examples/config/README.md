# Config Example

[`rail.toml`](rail.toml) shows every major configuration section in one file. Use it as a policy reference and copy only the sections the workspace needs.

## Validate it

```bash
cargo rail init
cargo rail config migrate --check
cargo rail config explain
cargo rail config validate --strict
```

`cargo rail init` writes only detected non-default choices. After an upgrade, `config migrate --check` reports explicit semantic migrations without changing the file; it never adds coded defaults.

## Reference

- [Configuration Reference](../../docs/config.md)
