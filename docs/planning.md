# Planning

`cargo rail plan` decides which registered work is required and emits the exact scope for each required item.
It does not execute repository commands.
Cargo, nextest, Just, scripts, and CI retain execution semantics.

A successful plan exits `0` whether work is required or skipped.
Invalid arguments, incompatible contracts, and operational failures exit `2`.

## Compare source states

By default, Cargo-Rail compares the default branch's merge base with the captured index, worktree,
and untracked files:

```bash
cargo rail plan
cargo rail plan --explain
cargo rail plan --json > plan.json
```

Use `--since <REF>` for an exact base.
Use `--from <REF> --to <REF>` for an exact Git-object comparison that ignores the live worktree.
`--all` is the only normal override; it requires every registered item and never narrows work.

One `WorkspaceContext` supplies the captured source, Cargo graph, dependency domains, effective configuration,
toolchain, targets, and compatible evidence.
Missing or incomplete evidence requires only the work item that owns the gap.

The work plan does not authorize release publication.
The [release transaction](release-records.md) retains its own exact source, validation attempts,
package and artifact digests, and external effects.
A skipped planner item cannot substitute for required release validation.

## Register repository work

Pure Cargo workspaces need no planning configuration.
Cargo-Rail already owns the built-in Cargo, dependency-policy, release-semver,
and Surface decisions.

Register only positive inputs for repository-specific work.
Keep commands in Just, scripts, or CI:

```toml
[plan.work.verification]
scope = "repository"
paths = ["verification/**"]
config = ["targets"]

[plan.work.deliverables]
scope = "variants"
paths = ["deliverables/**"]
variant_catalog = "variants.json"
```

| Scope        | Plan output                             | Use |
| ------------ | --------------------------------------- | --- |
| `repository` | Required or skipped                     | Gate a whole-repository command |
| `cargo`      | Typed package and target selection      | Pass the emitted Cargo arguments to a compatible command |
| `variants`   | Selected catalog rows or explicit `all` | Materialize only the emitted workflow variants |

Give each command's configuration files their own work item.
List the files each command reads, such as a test-runner or formatter configuration,
and route only the jobs that run that command on that item.
One broad work item that lists every configuration file runs every job when any one of them changes.
Built-in Cargo work already covers manifests, lockfiles, Cargo configuration, and the toolchain;
do not repeat them.
Check the routes with [`cargo rail plan --cases`](#check-route-parity-before-a-migration).

`cargo` subscriptions inherit the selected built-in Cargo scope.
A changed declared path adds its package scope.
A changed configuration input widens the declared work to the Cargo workspace
because policy can affect every member.

Variant catalog v2 models deliverable impact without subscribing the deliverable to conservative `cargo.build`.
Each row declares `cargo_roots` containing exact packages or targets and `external_paths` for inputs outside Cargo's graph.
A root may add `features` to name one exact no-default-feature set.
Cargo-Rail expands member-local Cargo feature edges
and follows ordinary external Rust modules from the captured target root.
Only a source path proved active for that feature set narrows selection; malformed attributes,
path overrides, inline modules, platform or custom cfgs,
and unattributed Rust files widen to every row.

An auxiliary root adds `manifest` with the exact repository-relative path of an entry in `release.auxiliary_cargo_manifests`.
Cargo-Rail loads its locked metadata from the same captured source,
keeps local packages inside that source root,
and applies the normal structural reverse build closure.
Auxiliary manifests, root lockfiles, shared inputs, and catalog changes remain all-row wideners.
Root Cargo manifests, the primary lockfile, Cargo configuration,
and the toolchain likewise select every Cargo-rooted row.

```json
{
  "package": "crypto",
  "features": ["signatures"]
}
```

```json
{
  "manifest": "fuzz-packages/rsa/Cargo.toml",
  "package": "fuzz-rsa"
}
```

Cargo-Rail computes structural impact once across the captured Cargo domains
and selects rows whose roots are affected.
Unrelated external paths do not select a deliverable merely
because compiler input evidence is incomplete.
If a required path, configuration input,
or Cargo input is not attributed by any selected catalog row,
Cargo-Rail selects every row rather than treating the gap as evidence
that a deliverable is unaffected.

Runtime artifacts remain separate from test execution.
A Cargo-scoped named work item with `cargo_prerequisites` emits only prerequisite packages and targets;
the source `cargo.test` selector still owns which tests execute.
Changing a declared artifact propagates back to its explicitly named test root.
Relationships are one hop and contain no commands.

Declare every runtime load.
Compiler evidence records compile-time inputs only.
It cannot see a test that opens a dynamic library, spawns an executable,
or reads a build output at run time.
With complete evidence and no declaration, a changed plugin selects only its own package,
and the test that loads it is skipped.
Without compatible evidence, that test is retained through conservative widening.
Cargo-Rail does not check that a test loads the declared artifact, and it does not order execution.
The consumer builds the `require` selection before it runs the source work.

Work IDs start with a lowercase ASCII letter and then use lowercase ASCII letters, digits, dots,
and hyphens.
Paths are positive repository-relative globs.
Absolute paths, parent traversal, negative patterns, commands, unknown configuration fields,
and malformed variant catalogs are rejected.

## Consume the machine contract

`cargo rail plan --json` writes one schema-owned value.
The checked-in contract is [`plan-v9.schema.json`](../schemas/plan-v9.schema.json):

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --json > plan.json
```

The fields with execution authority are:

- `identity`: the content-derived decision identity;
- `inputs`: comparison, source, Cargo, configuration, toolchain, platform, catalog, and evidence bindings;
- `work`: every tagged required or skipped decision;
- `required`: the sorted projection of required work IDs; and
- `work.NAME.scope`: the selector attached only to required work.

`changes`, causes, explanations, and evidence describe a decision; they are not selectors.
Read `work.NAME.scope.selection.cargo_args` as an argument array, never as shell text.
Treat variant `kind = "all"` as an instruction to materialize the owning workflow's complete checked-in catalog.

Before each executor:

1. Validate the complete plan and required-work projection.
1. Select one known required work item.
1. Lower only that item's typed scope.
1. Run the matching reader's checkout verification in the execution workspace.
1. Start the executor only after verification succeeds.

The companion GitHub Action owns the independent strict consumer.
It publishes the exact plan, reader, and Cargo-Rail version used to create the decision.
Every execution job installs that exact version
before the reader delegates checkout verification to Cargo-Rail.
Comparing `HEAD` alone is insufficient; drift exits `2` before selectors are emitted or work starts.
Readers that already captured a plan use `cargo rail plan --verify -` and pass those exact bytes on standard input
so verification cannot reopen a different pathname.

Verification binds the plan to the current platform, Cargo configuration, toolchain, workspace,
and source.
Another platform's `required` list can route a job,
but only a plan created on the job's platform passes verification there.
The checkout can move: the same commit at another path verifies,
but another workspace in the same repository does not.
Store a transferred plan outside the checkout; an unignored plan file inside it is untracked drift.

## Observed-input evidence

Without evidence, a changed file that a compiler, build script,
or procedural macro could read widens every built-in Cargo work item, even a README.
Record portable evidence from the ordinary build of the commit that later changes compare against:

```bash
cargo rail plan evidence --work cargo.test --output target/planning-evidence/cargo.test.json \
  -- test --workspace --locked --no-run
cargo rail plan --evidence target/planning-evidence/cargo.test.json --explain-work cargo.test
```

The command runs Cargo with a recorder as `RUSTC_WRAPPER`.
The recorder notes each workspace compiler invocation and runs the compiler unchanged, so outputs,
Cargo freshness, and any configured wrapper, such as the compiler cache, behave as usual.
Afterward Cargo-Rail reads the dep-info file rustc wrote for every workspace unit
and the rerun declarations of every build script.
A unit that Cargo reports as fresh keeps the dep-info of its last compile,
which is the input set Cargo just checked, so recording also works on a warm target directory.
Each input is bound to its Git object at `HEAD`.
The command fails without writing evidence when the build fails or a tracked file differs from `HEAD`.
Cargo diagnostics and test output go to standard error.

Record `cargo.build`, `cargo.clippy`, and `cargo.test`, one work item per run.
Use the packages, targets, features, and profile of the command the job runs:
evidence covers only what the recorded build compiled,
and the planner applies it to the whole work item.
Every workspace member must compile at least one unit, or the evidence is incomplete.
Writing to an existing file keeps its other work items when the bindings match.
Documentation, doctest, and packaging work have no recorder and keep widening.

The evidence follows Cargo's own rebuild model:

- A changed file selects every package whose units read it,
  plus the packages whose builds depend on them.
  A changed integration-test input selects only that test target.
- A build script's `rerun-if-changed` paths are inputs; a declared directory also covers files added to it.
  A script that declares no path depends on every file in its package.
- A procedural macro's reads count only when the macro reports them to the compiler,
  as Cargo requires.
  A macro that reads a file without reporting it can go stale under Cargo too.
- An added `clippy.toml` or `.clippy.toml` always selects `cargo.clippy`, because dep-info names only files that exist.
- Test runtime reads, such as a fixture a test opens, are not compiler inputs.
  Declare them as repository work.

Cargo-Rail marks the work item incomplete, and the planner keeps widening it,
when a unit's dep-info is unavailable, a unit reads an untracked file that Git does not ignore,
a unit reads a repository file outside the workspace,
a build script declares a missing path or a directory with no tracked files,
or an input names a secret-like environment variable.
The command lists what it could not observe.

Pass one `--evidence` per file.
[`planning-evidence-v2.schema.json`](../schemas/planning-evidence-v2.schema.json) binds each manifest to its source commit, Cargo universe,
Cargo configuration, toolchain, target, platform, provider capabilities, and work items.
Each file is validated separately.
A missing, stale, malformed, or foreign file, or two files for one work item,
widens only the work items without other compatible evidence.
The plan lists the identities of the manifests it used,
and `--explain-work` names the manifest behind each observed decision.

### Transfer evidence in CI

Record in the jobs that already build on the default branch, and save the file by commit:

```yaml
- name: Build tests and record planning evidence
  run: >
    cargo rail plan evidence --work cargo.test
    --output target/planning-evidence/cargo.test.json
    -- test --workspace --locked --no-run
- run: cargo test --workspace --locked
- uses: actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
  if: github.event_name == 'push'
  with:
    path: target/planning-evidence/cargo.test.json
    key: cargo-rail-evidence-${{ runner.os }}-${{ runner.arch }}-cargo.test-${{ github.sha }}
```

The later test run finds every unit fresh, so recording adds no compilation.
Before planning, restore the evidence of the comparison base.
For the default pull-request checkout, a merge commit, the base is the pull request's base commit:

```yaml
- uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
  with:
    path: target/planning-evidence/cargo.test.json
    key: cargo-rail-evidence-${{ runner.os }}-${{ runner.arch }}-cargo.test-${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before }}
```

Repeat the save and restore for each recorded work item,
and pass the directory to the [companion Action](https://github.com/loadingalias/cargo-rail-action)'s `evidence` input.
A cache miss, or evidence for a different base, only widens.
Keep the directory ignored by Git; an unignored file would be a changed path.

A plan identity compares decisions.
It is not a cache key and never authorizes compiler-result reuse.

The current planner accepts variant catalog v2.
Use the checked-in v2 schema when authoring a catalog.

## Diagnose a decision

```bash
cargo rail config validate --strict
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work}'
```

See [Troubleshooting](troubleshooting.md) when the compared states, selected work,
or executor scope differ from expectation.

### Impact attribution

Plan contract v9 adds `attribution`, keyed by exactly the required work IDs.
It records typed triggering inputs and one relation for every selected package or variant: `direct`, `dependency`, or `unattributed`.
Dependency relations name one captured originating package;
they do not claim to enumerate every causal path.
A directly changed package can also have affected dependencies
and remains direct in the presentation.

Attribution is bound into the canonical plan identity.
It explains the final selectors without changing their execution authority.
Workspace scope, incomplete evidence, and `--all` remain explicit.
Consumers must not reconstruct these relations from paths or the human evidence description.
The Action keeps direct selections visible and places dependency details in expandable sections.
CLI `--explain` includes the complete selected scope.

Cargo-Rail emits and verifies v9 plans.
Regenerate older saved plans and use a companion Action that independently validates v9 attribution.

## Check route parity before a migration

Before you replace path filters or another CI selector,
record the routes you expect and compare them with the planner:

```toml
[[case]]
name = "test runner configuration"
change = [".config/nextest.toml"]
required = ["test-runner"]
skipped = ["format-config"]

[[case]]
name = "unrelated file"
change = ["notes/new.txt"]
required = ["cargo.test"]
```

```bash
cargo rail plan --cases routes.toml
```

Each case appends `append` (default: one newline) to each repository-relative `change` path, or creates the file.
Cargo-Rail writes the result as an unreferenced commit on top of `HEAD` and plans it in object mode.
The worktree, index, refs, and untracked files do not change and do not affect a case.
`required` fails when the work is skipped, and names the missing work ID.
`skipped` fails when the work is required.
Set `precise = true` to fail when expected work is required only by conservative expansion.
The report lists every decision as `direct` or `expanded: incomplete evidence`, and the reasons for each expansion.
The command exits `1` when a case fails and `2` when the case file is invalid; it has no JSON output.

A passing case is diagnostic.
It never becomes evidence that work can be skipped.
Without compatible portable evidence, an unrelated file can still widen Cargo work,
because compiler macros, build scripts, and procedural macros can read repository files.
Record [observed-input evidence](#observed-input-evidence) to skip it; do not work around the expansion with path patterns.

Each check proves one claim:

| Command                                   | Proves |
| ----------------------------------------- | ------ |
| `cargo rail config validate --strict`     | Policy is valid for this workspace's Cargo graph. |
| `cargo rail plan --all --json`            | Every registered work item has valid full scope. It proves no routing decision. |
| `cargo rail plan --cases FILE`            | The reviewed path cases route as expected from `HEAD`. |
| `cargo rail plan --explain-work WORK_ID`  | Why one work item was required or skipped, including its paths and evidence. |
| `cargo rail plan evidence --work WORK_ID` | Which workspace files the recorded build read, bound to `HEAD`. |

Start with `--explain-work` when one job runs or skips unexpectedly.
