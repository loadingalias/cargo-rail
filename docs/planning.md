# Planning

`cargo rail plan` turns Git changes and Cargo's resolved graph into typed surface and package scope. Cargo,
cargo-nextest, Just, and CI consume that scope directly. They retain command semantics, argument ownership, output,
and exit behavior.

## How a plan is built

1. **Choose a comparison.** Worktree plans compare the captured source tree with `--since`, the default branch, or its
   merge base. `--from` and `--to` compare two Git objects.
2. **Interpret Cargo inputs.** Manifest and lockfile changes are compared semantically. Formatting and package metadata
   changes can select no package work. Unknown resolver, membership, source, parse, or removed-resolution changes
   widen to the workspace.
3. **Classify files.** Built-in rules select build, test, benchmark, documentation, or infrastructure surfaces.
   Configured globs add infrastructure and custom surfaces.
4. **Resolve ownership and impact.** Cargo metadata maps files to packages. One declared-dependency universe keeps
   build-transitive and development-only impact separate while accounting for optional and target-gated edges.
5. **Build surface scope.** Each package-scoped surface receives `empty`, `crates`, or `workspace` scope and the exact
   matching Cargo argument vector.

Generated source roots and Cargo-Rail state are excluded from change collection. Worktree plans use one captured
source and resolution view; they do not combine file decisions from one moment with Cargo decisions from another.

## Surfaces

| Surface | Meaning |
|---|---|
| `build` | Compilation may be affected |
| `test` | Test compilation or execution may be affected |
| `bench` | Benchmark work changed |
| `docs` | Documentation work changed |
| `infra` | CI, scripts, tooling, or workspace operations changed |
| custom name | A configured repository-specific classification matched |

`infra` and custom surfaces are gates. They do not carry Cargo package arguments because their commands belong in
Just, CI, or a purpose-built domain tool.

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

## Consume typed scope

Inspect the plan in text or as one machine-safe JSON value:

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
```

Use planner fields according to their ownership:

- `surfaces.NAME.enabled` and `.reasons` decide one surface.
- `surfaces.NAME.scope` is that surface's exact package handoff.
- `scope` is the compatibility union of active package-scoped surfaces.
- `resolution_universe` identifies the dependency semantics used by every surface.
- `trace` is the stable reason chain.
- `impact` explains graph propagation; it is not execution scope.

Pass `surfaces.NAME.scope.cargo_args` as an argument array. Do not join it into a shell command or rebuild it from
`impact`:

```bash
PLAN_JSON=$(cargo rail plan --merge-base -f json)

if [ "$(jq -r '.surfaces.test.enabled' <<<"$PLAN_JSON")" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r argument; do
    CARGO_ARGS+=("$argument")
  done < <(jq -r '.surfaces.test.scope.cargo_args[]' <<<"$PLAN_JSON")
  cargo nextest run "${CARGO_ARGS[@]}" --all-features --locked
fi
```

The consumer must validate the contract versions it supports. This repository's scripts reject an unknown planner or
scope contract before executing Cargo.

## Confidence and incomplete evidence

| Confidence | Behavior |
|---|---|
| `strict` | Expands owned changes to build/test and propagates their impact |
| `balanced` | Uses built-in classification and the configured unknown-file policy |
| `fast` | Avoids conservative transitive expansion |

`unknown_file_policy` controls unrecognized paths independently. See the
[configuration reference](config.md#change-detection) for its policies and defaults.

`--from A --to B` cannot ask Cargo to resolve a tree that is not checked out. If package work is selected, the planner
widens it to workspace scope and records `historical_resolution_unavailable`. Current-graph edges may remain as
explanation, but they do not narrow historical scope.

Worktree plans derive one command-independent universe from captured declarations. Optional and target-gated edges
can propagate impact regardless of default features or the host target.

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

Exit code `0` means success, and `2` means an operational or argument error. Commands with an explicit check contract
use `1` for changes required.

`cargo rail hash` emits a root-independent `plan-v1:sha256:<digest>` over the planning decision. It excludes local
diagnostics such as the absolute workspace root. The digest compares decisions; it is not a cache key and never
authorizes result reuse.

## GitHub Actions

[`loadingalias/cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) installs Cargo-Rail, runs the
planner once, validates planner and scope versions, and exports their projections. Action major v6 consumes planner v6
and scope v4; its `version` input independently selects the Cargo-Rail release.

```yaml
- uses: loadingalias/cargo-rail-action@v6
  id: rail
  with:
    mode: debug

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  shell: bash
  env:
    PLAN_JSON: ${{ steps.rail.outputs.plan-json }}
  run: |
    CARGO_ARGS=()
    while IFS= read -r argument; do
      CARGO_ARGS+=("$argument")
    done < <(jq -r '.surfaces.test.scope.cargo_args[]' <<<"$PLAN_JSON")
    cargo nextest run "${CARGO_ARGS[@]}" --locked
```

Minimal mode exports built-in surface booleans, `surfaces-json`, `scope-json`, `cargo-args`, and `base-ref`. Debug mode
also exports the full `plan-json`. Pin the action to a full commit SHA when immutable third-party action execution is
required.

Transparent local reuse remains independent of planning. Run `cargo rail cache setup` once on an execution machine;
ordinary Cargo and nextest processes using that Cargo home can then use verified L1. See
[Share local compiler reuse across workspaces](cache-sharing.md).

## Diagnose

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json | jq '.surfaces'
```

See [Troubleshooting](troubleshooting.md) when a surface, package, or scope differs from expectation.
