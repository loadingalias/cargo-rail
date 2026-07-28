# Change Detection: Standalone

Use this pattern when CI should pass planner-selected package arguments directly to Cargo without a repository-specific task runner.

## Local Example

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --action test
cargo rail run --merge-base --dry-run --print-cmd
```

## CI Example

```yaml
- uses: loadingalias/cargo-rail-action@v6.0.0
  id: rail
  with:
    version: 0.19.1

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  env:
    CARGO_ARGS: ${{ steps.rail.outputs.cargo-args }}
  run: cargo test $CARGO_ARGS

- name: Build selected packages
  if: steps.rail.outputs.build == 'true'
  env:
    CARGO_ARGS: ${{ steps.rail.outputs.cargo-args }}
  run: cargo build $CARGO_ARGS
```

## Built-in Actions

- `build`
- `test`
- `bench`
- `docs`
- `format`
- `lint`
- `msrv`
- `package`
- `audit`
- `distribution`

`infra` remains a planner surface for gating configured repository actions; it is not executable itself.

Use `cargo rail run` instead when profiles, test-runner selection, or decision receipts should be owned by Rail.
