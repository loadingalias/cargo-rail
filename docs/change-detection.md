# Change Detection

`cargo-rail` classifies changed files into surfaces and then projects an execution scope.

## Surfaces

| Surface | Meaning |
|---|---|
| `build` | compilation work changed |
| `test` | test execution may be affected |
| `bench` | benchmark work changed |
| `docs` | documentation-only work changed |
| `infra` | CI, scripts, tooling, or workspace-level operations changed |

## Basic Commands

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --surface test
```

Use `plan` when you want the contract. Use `run` when you want execution.

## Important Outputs

- `surfaces` tells you which surfaces are active
- `trace` tells you why
- `impact` is diagnostic
- `scope` is the execution handoff

If another tool needs package selection, use `scope`, not `impact`.

## Config

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock", "rust-toolchain.toml"]
confidence_profile = "balanced"
bot_pr_confidence_profile = "strict"
unknown_file_policy = "strict"

[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
```

## Confidence Profiles

| Profile | Intent |
|---|---|
| `strict` | prefer running more over missing impact |
| `balanced` | default tradeoff |
| `fast` | minimize execution expansion |

## Common CI Pattern

```yaml
- uses: loadingalias/cargo-rail-action@v4
  id: rail

- name: Run selected CI profile
  if: steps.rail.outputs.build == 'true' || steps.rail.outputs.test == 'true'
  run: cargo rail run --since "${{ steps.rail.outputs.base-ref }}" --profile ci
```

## Validate

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd
```

## See Also

- [Configuration Reference](./config.md)
- [Troubleshooting](./troubleshooting.md)
