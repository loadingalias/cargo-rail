# Planning

`cargo rail plan` turns Git changes and Cargo's resolved graph into typed surface and package scope. Cargo,
cargo-nextest, Just, and CI consume that scope while retaining their command semantics and exit behavior.

## How a plan is built

1. **Choose a comparison.** Worktree plans compare the captured source tree with `--since`, the default branch, or its
   merge base. `--from` and `--to` compare two Git objects.
2. **Interpret Cargo inputs.** Manifest and lockfile changes are compared semantically. Formatting and package metadata
   changes can select no package work. Unknown resolver, membership, source, parse, or removed-resolution changes
   widen to the workspace.
3. **Classify files.** Built-in rules select build, test, benchmark, documentation, infrastructure, and configured
   source-surface analysis. Configured globs add infrastructure and custom surfaces.
4. **Resolve ownership and impact.** Cargo metadata maps files to packages. One declared-dependency universe keeps
   build-transitive and development-only impact separate while accounting for optional and target-gated edges.
5. **Build surface scope.** Each package-scoped surface receives `empty`, `crates`, or `workspace` scope and the exact
   matching Cargo argument vector.

Generated source roots and Cargo-Rail state are excluded. Worktree plans use one captured source and resolution view.

## Surfaces

| Surface     | Meaning                                                 |
| ----------- | ------------------------------------------------------- |
| `build`     | Compilation may be affected                             |
| `test`      | Test compilation or execution may be affected           |
| `bench`     | Benchmark work changed                                  |
| `docs`      | Documentation work changed                              |
| `infra`     | CI, scripts, tooling, or workspace operations changed   |
| `surface`   | Rust source or Surface policy may require analysis      |
| custom name | A configured repository-specific classification matched |

`infra`, `surface`, and custom surfaces are gates. They do not carry Cargo package arguments because their commands
belong in Just, CI, or a purpose-built domain tool. The native `surface` gate is selected only when
`[surface] enabled = true` and a relevant change requires whole-workspace analysis.

```toml
[change-detection]
infrastructure = [".github/**", "scripts/**", "Cargo.lock"]
confidence_profile = "balanced"
unknown_file_policy = "strict"

[change-detection.custom]
benchmarks = ["benches/**", "perf/**"]
```

Cargo ownership comes from the resolved graph. Globs classify repository surfaces; they do not define package
ownership.

## Consume typed scope

Inspect the plan in text or as one machine-safe JSON value:

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
```

Use planner fields according to their ownership:

- `surfaces.NAME.enabled` and `.reasons` decide one surface.
- `surfaces.NAME.scope` is the exact package handoff for package-scoped surfaces and empty for gate-only surfaces.
- `scope` is the compatibility union of active package-scoped surfaces.
- `resolution_universe` identifies the dependency semantics used by every surface.
- `trace` is the stable reason chain.
- `impact` explains graph propagation; it is not execution scope.

Pass `surfaces.NAME.scope.cargo_args` as an argument array. Do not join it into a shell command or reconstruct it from
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

Route the whole-workspace Surface gate from its planner decision:

```bash
if [ "$(jq -r '.surfaces.surface.enabled' <<<"$PLAN_JSON")" = "true" ]; then
  cargo rail surface --check
fi
```

The execution machine must have a supported native release archive and its adjacent compiler-fact driver. See
[Configuration](config.md#surface) for the closed-world and lint policy.

Consumers must validate supported contract versions. This repository rejects unknown planner or scope contracts
before executing Cargo.

## Confidence and incomplete evidence

| Confidence | Behavior                                                            |
| ---------- | ------------------------------------------------------------------- |
| `strict`   | Expands owned changes to build/test and propagates their impact     |
| `balanced` | Uses built-in classification and the configured unknown-file policy |
| `fast`     | Avoids conservative transitive expansion                            |

`unknown_file_policy` controls unrecognized paths independently. See the
[configuration reference](config.md#change-detection) for its policies and defaults.

`--from A --to B` cannot ask Cargo to resolve a tree that is not checked out. If package work is selected, the planner
widens it to workspace scope and records `historical_resolution_unavailable`. Current-graph edges may remain as
explanation, but they do not narrow historical scope.

Worktree plans derive one command-independent universe from captured declarations. Optional and target-gated edges
can propagate impact regardless of default features or host target.

## Machine contract

`cargo rail plan -f json` is a versioned API. The current output is planner contract v7 and scope contract v4 inside
machine envelope v1:

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --merge-base -f json > plan.json
```

[`schemas/plan-v7.schema.json`](../schemas/plan-v7.schema.json) is the checked-in schema. `plan --schema` runs before
workspace context construction, so a consumer can check compatibility without opening a repository.

Compatibility rules:

- Adding an optional field is backward-compatible.
- Removing a field, changing its type or meaning, requiring an optional field, or changing a reason code requires a
  new planner contract and schema.
- The envelope, planner, and scope versions evolve independently; check every version you consume.
- `trace.code` is stable machine data. `description` is human text.
- Successful structured output is one stdout value with no progress contamination.

Exit code `0` means success. Exit code `2` means an argument or operational error. Commands with a check contract use
`1` for required changes.

`cargo rail hash` emits a root-independent `plan-v1:sha256:<digest>` over the planning decision. It excludes local
diagnostics such as the absolute workspace root. The digest compares decisions; it is not a cache key and never
authorizes result reuse.

## GitHub Actions

[`loadingalias/cargo-rail-action`](https://github.com/loadingalias/cargo-rail-action) installs Cargo-Rail, runs the
planner once, validates planner and scope versions, and exports their projections. The pinned revision below consumes
planner v7 and scope v4; the action's `version` input independently selects a compatible Cargo-Rail release.

```yaml
- uses: loadingalias/cargo-rail-action@47e86bde928ce420b85efa5f8d3b5feb96fd0ffc # v7.0.0
  id: rail
  with:
    version: 0.23.0

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  shell: bash
  env:
    SCOPE_JSON: ${{ steps.rail.outputs.scope-json }}
  run: |
    CARGO_ARGS=()
    while IFS= read -r argument; do
      CARGO_ARGS+=("$argument")
    done < <(jq -r '.cargo_args[]' <<<"$SCOPE_JSON")
    cargo nextest run "${CARGO_ARGS[@]}" --locked
```

Minimal mode exports built-in surface booleans, `surfaces-json`, `scope-json`, `cargo-args`, and `base-ref`. Debug mode
also exports a same-job `plan-file`. Use `surfaces-json` for routing and `scope-json` for execution. The full plan is
not a GitHub output because a large diff can exceed the operating system's environment-size limit. Read `plan-file`
in the same job or transfer it as an artifact. Pin the action to a full commit SHA for immutable execution.

Compiler reuse is independent of planning. Run `cargo rail cache setup` once on an execution machine. Cargo and
nextest processes using that Cargo home can then use verified L1. See
[Share local compiler reuse across workspaces](cache-sharing.md).

## Diagnose

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json | jq '.surfaces'
```

See [Troubleshooting](troubleshooting.md) when a surface, package, or scope differs from expectation.
