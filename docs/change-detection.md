# Change Detection

`cargo-rail` maps changed files to workspace crates, includes affected dependents, classifies CI surfaces, and emits the package scope that should run.

## Surfaces

| Surface | Meaning |
|---|---|
| `build` | compilation work changed |
| `test` | test execution may be affected |
| `bench` | benchmark work changed |
| `docs` | documentation-only work changed |
| `infra` | CI, scripts, tooling, or workspace-level operations changed |

## Plan and run

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --surface test
```

Use `plan` when you want the contract. Use `run` when you want execution.

## Execution outputs

- `surfaces` tells you which surfaces are active
- `trace` tells you why
- `impact` is diagnostic
- `scope` is the execution handoff
- `scope.cargo_args` is the Cargo package selection for that handoff

If another tool needs package selection, use `scope.cargo_args`, not `impact`.

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

## GitHub Actions

```yaml
- uses: loadingalias/cargo-rail-action@v5.0.0
  id: rail
  with:
    version: 0.17.0

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  env:
    CARGO_ARGS: ${{ steps.rail.outputs.cargo-args }}
  run: cargo test $CARGO_ARGS
```

Raw `cargo rail plan -f github` writes the same package selection as `cargo_args`. `@v5` selects the action contract; `version` selects the installed cargo-rail release.

## Validate

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd
```

## See Also

- [Configuration Reference](./config.md)
- [Troubleshooting](./troubleshooting.md)
