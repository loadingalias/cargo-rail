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
cargo rail run --merge-base --action test
```

Use `plan` when you want the contract. Use `run` when you want execution.

`run` expands every enabled built-in or configured repository task into an ordered, shell-free argv action before
executing the first action. A
successful run or dry run writes a version-2 decision receipt under `target/cargo-rail/receipts/run-decision-*.json`.
The receipt binds the action list to the workspace snapshot and records each action's exact argv array, logical working
directory, selected packages/targets/features, dependencies, typed environment names, declared side effects, and
planner trace reasons. `--dry-run` previews that same expansion without starting an action process. `--dry-run -f json`
and `-f github` expose the same deterministic topological order to CI. Current built-ins inherit the host environment
and are not sandboxed, so receipts mark their inputs and outputs as ambient and the actions as non-reusable.

Generated repository actions declare one output owner plus separate direct argv for read-only checking and
regeneration. Select `--generated check` to expose staleness, use the default `regenerate` mode to update files, and add
`--explain` to see output ownership.

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
- uses: loadingalias/cargo-rail-action@v5.1.0
  id: rail
  with:
    version: 0.18.0

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
