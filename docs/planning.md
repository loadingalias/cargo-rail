# Planning and Execution

`cargo rail plan` decides what changed and what work is affected. `cargo rail run` consumes that decision and executes
selected actions. They are one protocol: integrations must consume planner scope rather than recreate Cargo package
selection from path filters.

## End-to-end flow

1. **Choose a comparison.** Worktree plans compare the captured source tree with `--since`, the default branch, or its
   merge base. `--from` and `--to` compare two Git objects.
2. **Interpret Cargo inputs.** Manifest and lockfile changes are compared semantically. Formatting and package metadata
   changes can select no package work; workspace inheritance and lockfile changes are localized when the evidence is
   complete. Unknown resolver, membership, source, parse, or removed-resolution changes widen to the workspace.
3. **Classify files.** Built-in rules select build, test, benchmark, documentation, or infrastructure surfaces.
   Configured globs add infrastructure and custom surfaces.
4. **Resolve ownership and impact.** Cargo metadata maps files to packages. The planner builds one declared-dependency
   universe where optional and target-gated edges can participate, then keeps build-transitive and development-only
   impact separate.
5. **Build execution scopes.** Each package-scoped surface receives `empty`, `crates`, or `workspace` scope and the
   matching Cargo arguments.
6. **Expand actions.** `run` selects built-in or configured actions, consumes each surface's final package scope,
   orders dependencies, validates paths and outputs, binds exact Cargo resolution evidence, and then starts direct
   process argv.

Generated source roots and Cargo-Rail state are excluded from change collection. Worktree plans use one captured source
and resolution view; they do not combine file decisions from one moment with Cargo decisions from another.

## Surfaces and actions

A surface describes affected work. An action is an executable task.

| Surface | Meaning |
|---|---|
| `build` | Compilation may be affected |
| `test` | Test compilation or execution may be affected |
| `bench` | Benchmark work changed |
| `docs` | Documentation work changed |
| `infra` | CI, scripts, tooling, or workspace operations changed |
| `custom:NAME` | A configured repository-specific classification matched |

`infra` and `custom:*` are planner outputs, not action IDs. Use them in a configured action's `when` policy:

```toml
[run.action.benchmark-report]
argv = ["cargo", "run", "-p", "xtask", "--", "benchmark-report"]
when = ["custom:benchmarks"]
inputs = ["benches"]

[run.action.benchmark-report.environment]
inherit = true

[run.profile.bench]
actions = ["bench", "benchmark-report"]
```

## Use the plan

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --dry-run --print-cmd --explain
cargo rail run --merge-base --profile ci
```

`plan` never executes selected actions. `run --dry-run` expands the same ordered action graph without spawning its
processes. JSON and GitHub run formats require `--dry-run`.

Use planner fields according to their ownership:

- `surfaces.NAME.enabled` and `.reasons` are the decision for one surface.
- `surfaces.NAME.scope` is the exact package handoff for that surface.
- `scope` is the compatibility union of active package-scoped surfaces.
- `resolution_universe` identifies the declared dependency semantics used to compute every surface scope.
- `trace` is the stable reason chain.
- `impact` explains graph propagation; it is not an execution scope.

An integration that executes one surface should use `surfaces.NAME.scope.cargo_args`. Integrations that intentionally
combine all active package-scoped work may use `scope.cargo_args`.

Every successful run or dry run writes a version-4 decision receipt under
`target/cargo-rail/receipts/run-decision-*.json`. It binds the ordered action graph, argv, working directories,
packages, targets, features, typed environment policy, side effects, and planner reasons to the workspace snapshot.

## Confidence and repository policy

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock"]
confidence_profile = "balanced"
unknown_file_policy = "strict"

[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
```

Cargo ownership always comes from the resolved graph. Globs classify repository surfaces; they do not replace package
ownership.

| Confidence | Behavior |
|---|---|
| `strict` | Expands owned changes to build/test and propagates their impact |
| `balanced` | Uses the built-in classification and conservative unknown-file policy |
| `fast` | Avoids conservative transitive expansion |

`unknown_file_policy` controls unrecognized paths independently. See the
[configuration reference](config.md#change-detection) for its four policies and defaults.

## Historical comparisons

`--from A --to B` cannot ask Cargo to resolve a tree that is not checked out. If package work is selected, the planner
widens it to workspace scope and records `historical_resolution_unavailable`. Current-graph edges may still appear as
explanation, but they do not narrow historical execution.

Worktree plans derive one command-independent universe from captured declarations. Every optional and target-gated
edge can propagate impact regardless of default features or the host target. `run` records each action's exact feature
and target resolution but does not change planner scope.

## Machine contract

`cargo rail plan -f json` is a versioned API. The current output is planner contract v6 and scope contract v4 inside
machine envelope v1:

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --merge-base -f json > plan.json
```

[`schemas/plan-v6.schema.json`](../schemas/plan-v6.schema.json) is the checked-in schema. `plan --schema` runs before
workspace context construction, so a consumer can check compatibility without opening a repository.

Compatibility rules:

- Adding an optional field is backward-compatible.
- Removing a field, changing its type or meaning, requiring an optional field, or changing a reason code requires a
  new planner contract and schema.
- The envelope, planner, and scope versions evolve independently; check every version you consume.
- `trace.code` is stable machine data. `description` is human text.
- Successful structured output is one stdout value with no progress contamination.

Exit code `0` means success or clean, `1` means a check found required changes, and `2` means an operational or argument
error, except deliberate subprocess propagation.

`cargo rail hash` emits a root-independent `plan-v1:sha256:<digest>` over the execution-relevant planner decision.
It excludes local diagnostics such as the absolute workspace root. The digest compares decisions; it is not a cache
key and never authorizes result reuse.

## GitHub Actions

[`loadingalias/cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) installs Cargo-Rail, runs the
planner once, validates the planner and scope versions, and exports their projections. Action major v6 consumes planner
v6 and scope v4; its `version` input independently selects the Cargo-Rail release.

```yaml
- uses: loadingalias/cargo-rail-action@v6
  id: rail
  with:
    version: 0.22.0

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  env:
    CARGO_ARGS: ${{ steps.rail.outputs.cargo-args }}
  run: cargo test $CARGO_ARGS
```

Minimal mode exports the built-in surface booleans, `surfaces-json`, `scope-json`, `cargo-args`, and `base-ref`. Debug
mode also exports the full `plan-json`. Pin the action to a full commit SHA when immutable third-party action execution
is required.

The direct `cargo test` handoff above consumes Cargo-Rail's package scope. It can also use transparent local L1 after
the execution machine runs `cargo rail cache setup`; the planner Action does not install or activate that machine
authority. See [Share local compiler reuse across workspaces](cache-sharing.md). The planner Action remains a job gate
and contract transport, not a process-wide Cargo wrapper.

## Diagnose and validate

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd --explain
```

See [Troubleshooting](troubleshooting.md) when a surface, package, or action differs from expectation.
