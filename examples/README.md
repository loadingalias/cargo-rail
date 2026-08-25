# Examples

Each directory shows one workflow. Every `rail.toml` is valid from this repository root.

| Workflow              | Example                   | What it covers                                    |
| --------------------- | ------------------------- | ------------------------------------------------- |
| Affected CI           | [planning](planning/)     | Direct Cargo execution or an existing task runner |
| Dependency policy     | [unify](unify/)           | Every `[unify]` field                             |
| Rust visibility       | [surface](surface/)       | Every Surface field and policy entry              |
| Releases              | [release](release/)       | Release, changelog, filters, and crate overrides  |
| Repository extraction | [split-sync](split-sync/) | Every split and sync mapping field                |

Validate one example with:

```bash
cargo rail --config examples/unify/rail.toml config validate --strict
```

Use the [configuration reference](../docs/config.md) for every allowed value. Copy only the fields that express real
workspace policy; omitted fields use documented defaults.
