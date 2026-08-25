# Rust Visibility

[`rail.toml`](rail.toml) shows every `[surface]` field and every policy-entry shape. Replace the illustrative product,
external boundary, override, and exclusion with facts from your workspace.

From the repository root:

```bash
cargo rail --config examples/surface/rail.toml config validate --strict
cargo rail --config examples/surface/rail.toml surface --prepare
cargo rail --config examples/surface/rail.toml surface --check --explain
```

Surface merges compiler views before deciding whether a declaration is unused or too visible. In plain terms: it
checks the whole house before declaring a door unnecessary. Keep `consumer_scope = "open"` unless the workspace owns
every consumer of its private packages.

The complete native installer includes authenticated Surface driver authority. `surface --prepare` installs the exact
Cargo-selected rustup toolchain component when absent and authenticates the selected producer before analysis. Source
installs can print `surface --schema`, but cannot prepare or analyze code. See
[Installation](../../README.md#installation) and the
[configuration reference](../../docs/config.md#surface).
