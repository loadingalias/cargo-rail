# Change Detection

Cargo-Rail maps changed files to the crates that own them, adds the crates that depend on those, classifies what kind of
work the change requires, and emits the exact package scope that should run. Crate ownership and reverse impact come
from Cargo's resolved workspace graph. Configured globs classify infrastructure and custom surfaces only; they do not
stand in for the package graph.

`plan` is the scope authority. `run`, JSON/GitHub projections, and external integrations consume that scope instead of
reimplementing package selection.

## Surfaces

A surface is a kind of work a change can require:

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

`plan` reports the decision without executing anything. `run` executes it.

Before starting the first process, `run` expands every enabled built-in or configured repository task into an
ordered list of direct process invocations — exact argv arrays, no shell. `--dry-run` previews that same expansion
without starting any process; `--dry-run -f json` and `-f github` expose the same deterministic topological order
to CI.

Each Cargo action resolves its own target/feature view and records the exact root `PackageId`s and resolved-node
count for it, so default, all-feature, named-feature, host, and cross-target actions never silently share one
maximal dependency graph.

A successful run or dry run writes a version-4 decision receipt under
`target/cargo-rail/receipts/run-decision-*.json`. The receipt binds the action list to the workspace snapshot and
records, for every action: the exact argv array, logical working directory, selected packages/targets/features,
dependencies, typed environment names, declared side effects, and the planner trace reasons that selected it.
Current built-in actions inherit the host environment and are not sandboxed, so action-key analysis explains the
ambient input and output boundaries and withholds a reusable cache key.

Generated repository actions declare one output owner plus separate direct argv for read-only checking and
regeneration. Select `--generated check` to expose staleness, use the default `regenerate` mode to update files,
and add `--explain` to see output ownership.

## Execution outputs

- `surfaces` tells you which surfaces are active
- `trace` tells you why
- `impact.build_transitive_crates` and `impact.development_transitive_crates` expose semantic graph impact
- `scope` is the compatibility handoff across all active surfaces
- `surfaces.NAME.scope` is the exact package selection for one surface
- `scope.cargo_args` is the compatibility Cargo selection across active package-scoped surfaces

If another tool executes one surface, use `surfaces.NAME.scope.cargo_args`, not `impact` — impact is diagnostic
context, not an execution scope. Existing integrations that execute all active package-scoped surfaces together may
continue using `scope.cargo_args`.

## Worktree versus historical plans

When planning the current worktree, Cargo-Rail resolves Cargo's default features for every effective Cargo build target,
defaulting to the host. Reverse impact preserves normal, development, build, proc-macro, optional, renamed, and
target-conditioned edges.

Historical object-to-object planning — comparing two commits with neither checked out — cannot ask Cargo to resolve
the historical tree, so package-scoped work widens to the workspace. Current-graph edge evidence is still shown,
but as explanation only, reported as `historical_resolution_unavailable` in `trace` and `--explain`.

## Config

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock", "rust-toolchain.toml"]
confidence_profile = "balanced"
unknown_file_policy = "strict"

[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
```

## Confidence profiles

| Profile | Intent |
|---|---|
| `strict` | prefer running more over missing impact |
| `balanced` | default tradeoff |
| `fast` | minimize execution expansion |

## GitHub Actions

```yaml
- uses: loadingalias/cargo-rail-action@v6
  id: rail
  with:
    version: 0.20.0

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  env:
    CARGO_ARGS: ${{ steps.rail.outputs.cargo-args }}
  run: cargo test $CARGO_ARGS
```

Raw `cargo rail plan -f github` writes the same package selection as `cargo_args`. `@v6` selects the action contract; `version` selects the installed cargo-rail release.

## Validate

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd
```

## See also

- [Configuration Reference](./config.md)
- [Troubleshooting](./troubleshooting.md)
