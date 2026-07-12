# Change Detection: Standalone

Use this pattern when CI should pass planner-selected package arguments directly to Cargo without a repository-specific task runner.

## Local Example

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --surface test
cargo rail run --merge-base --dry-run --print-cmd
```

## CI Example

```yaml
- uses: loadingalias/cargo-rail-action@v5.1.0
  id: rail
  with:
    version: 0.17.1

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

## Built-in Surfaces

- `build`
- `test`
- `bench`
- `docs`
- `infra` for gating only

Use `cargo rail run` instead when profiles, test-runner selection, or decision receipts should be owned by cargo-rail.
