# Troubleshooting

Use this when planner or runner behavior is not what you expected.

## Why did this run?

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --dry-run --print-cmd --explain
```

Check:

- `surfaces.*.enabled`
- `surfaces.*.reasons`
- `scope.mode`
- `scope.crates`

Use `scope` for execution decisions. Use `impact` for diagnostic context.

## Why did this not run?

Check these in order:

1. wrong base ref
2. wrong surface or profile
3. confidence profile behavior
4. path classification or custom rules
5. binary-only crate filtering via `--ignore-bin-crates`

## Common mistakes

### Custom surfaces in run profiles

Custom surfaces are planner outputs, not valid `run.profile.*.surfaces` entries.

```toml
# wrong
[run.profile.bench]
surfaces = ["custom:benchmarks"]

# right
[run.profile.bench]
surfaces = ["bench"]
```

### CI/local mismatch

Use the same base-ref strategy and profile in both places.

```bash
# local
cargo rail run --merge-base --profile ci

# CI
cargo rail run --since "$RAIL_SINCE" --profile ci
```
