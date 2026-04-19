# Change Detection: Standalone

Use this pattern when `cargo-rail` should handle both planning and execution.

## Local Example

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --surface test
cargo rail run --merge-base --dry-run --print-cmd
```

## CI Example

```yaml
- uses: loadingalias/cargo-rail-action@v4
  id: rail

- name: Run selected tests
  if: steps.rail.outputs.test == 'true'
  run: cargo rail run --since "${{ steps.rail.outputs.base-ref }}" --surface test

- name: Run selected build
  if: steps.rail.outputs.build == 'true'
  run: cargo rail run --since "${{ steps.rail.outputs.base-ref }}" --surface build
```

## Built-in Surfaces

- `build`
- `test`
- `bench`
- `docs`
- `infra` for gating only
