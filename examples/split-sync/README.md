# Split and Sync

[`rail.toml`](rail.toml) shows every `[crates.NAME.split]` field for one crate. `include` adds non-Cargo files;
`exclude` can only narrow that list. Cargo-owned package files come from workspace ownership automatically.

From the repository root:

```bash
cargo rail --config examples/split-sync/rail.toml config validate --strict
cargo rail --config examples/split-sync/rail.toml split run cargo-rail --check
```

Review the check result before running `split run` without `--check`. Later, use `sync --to-remote` or
`sync --from-remote`. A conflicted sync prints the receipt required by `sync --resume`.

For a combined split, set `mode = "combined"` and list at least two package names in `members`.

See the [configuration reference](../../docs/config.md#split-and-sync) for ownership and path rules.
