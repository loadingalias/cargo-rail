# Config Example

[`rail.toml`](rail.toml) shows every major configuration section in one file. Use it as a policy reference and copy only the sections the workspace needs.

## Validate it

```bash
cargo rail init
cargo rail config sync
cargo rail config validate --strict
```

`cargo rail init` writes the minimal detected configuration. Run `config sync` after every cargo-rail upgrade so new fields and detected targets are explicit before CI or mutation commands run.

## Reference

- [Configuration Reference](../../docs/config.md)
