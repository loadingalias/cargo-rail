# Planning

`cargo rail plan` decides which registered work is required.
It emits the exact scope for each required item.
It does not execute repository commands.
Cargo, nextest, Just, scripts, and CI keep the execution semantics.

A successful plan exits `0` whether work is required or skipped.
Invalid arguments, incompatible contracts, and operational failures exit `2`.

## Compare source states

By default, Cargo-Rail compares the merge base of the default branch with the captured index,
worktree, and untracked files:

```bash
cargo rail plan
cargo rail plan --explain
cargo rail plan --json > target/plan.json
```

The default branch is the first of these refs that exists: `origin/HEAD`, `origin/main`, `origin/master`, `main`, or `master`.
When none of these refs exists, `plan` fails.
Use `--since <REF>` for an exact base.
Use `--from <REF> --to <REF>` for an exact Git-object comparison that ignores the live worktree.
`--all` is the only normal override.
It requires every registered item and never narrows work.

One `WorkspaceContext` supplies the captured source, Cargo graph, dependency domains, effective configuration,
toolchain, targets, and compatible evidence.
Missing or incomplete evidence requires only the work item that owns the gap.

The work plan does not authorize release publication.
A [release](releases.md) requires the checks that GitHub reports on its own source commit.
A skipped planner item cannot replace those checks.

## Register repository work

Pure Cargo workspaces need no planning configuration.
Cargo-Rail already owns the built-in Cargo, `dependency-policy`, `release.semver`, and `surface` decisions.

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

Give the configuration files of each command their own work item.
In that item, list the files that the command reads,
such as a test-runner or formatter configuration.
Route only the jobs that run that command on that item.
If one broad work item lists every configuration file, a change to any one of them runs every job.
Built-in Cargo work already covers manifests, lockfiles, Cargo configuration, and the toolchain.
Do not repeat them.
Check the routes with [`cargo rail plan --cases`](#check-route-parity-before-a-migration).

`cargo` subscriptions inherit the selected built-in Cargo scope.
A changed declared path adds its package scope.
A changed configuration input widens the declared work to the Cargo workspace,
because policy can affect every member.

Variant catalog v2 models the impact on a deliverable.
It does not subscribe the deliverable to conservative `cargo.build`.
Each row declares `cargo_roots` and `external_paths`.
`cargo_roots` contains exact packages or targets.
`external_paths` contains the inputs outside the Cargo graph.
A root can add `features` to name one exact no-default-feature set.
Cargo-Rail expands member-local Cargo feature edges.
It follows ordinary external Rust modules from the captured target root.
Only a source path that is proved active for that feature set narrows the selection.
Malformed attributes, path overrides, inline modules, platform or custom cfgs,
and unattributed Rust files widen the selection to every row.

An auxiliary root adds `manifest`.
Its value is the exact repository-relative path of an entry in `release.auxiliary_cargo_manifests`.
Cargo-Rail loads the locked metadata of that manifest from the same captured source.
It keeps local packages inside that source root.
It applies the normal structural reverse build closure.
A change to an auxiliary manifest, a root lockfile, a shared input,
or the catalog still widens the selection to every row.
Root Cargo manifests, the primary lockfile, Cargo configuration,
and the toolchain also select every Cargo-rooted row.

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

Cargo-Rail computes structural impact one time across the captured Cargo domains.
It selects the rows whose roots are affected.
Unrelated external paths do not select a deliverable only
because compiler input evidence is incomplete.
If no selected catalog row attributes a required path, configuration input, or Cargo input,
Cargo-Rail selects every row.
It does not treat the gap as evidence that a deliverable is unaffected.

Runtime artifacts stay separate from test execution.
A Cargo-scoped named work item with `cargo_prerequisites` emits only prerequisite packages and targets.
The source `cargo.test` selector still owns which tests execute.
A change to a declared artifact propagates back to its explicitly named test root.
Relationships are one hop and contain no commands.

Declare every runtime load.
Compiler evidence records compile-time inputs only.
It cannot see a test that opens a dynamic library, spawns an executable,
or reads a build output at run time.
If the evidence is complete and no declaration exists,
a changed plugin selects only its own package.
The test that loads the plugin is then skipped.
Without compatible evidence, conservative widening keeps that test.
Cargo-Rail does not check that a test loads the declared artifact.
It does not order execution.
The consumer builds the `require` selection before it runs the source work.

Work IDs start with a lowercase ASCII letter.
The other characters are lowercase ASCII letters, digits, dots, and hyphens.
Paths are positive repository-relative globs.
Cargo-Rail rejects absolute paths, parent traversal, negative patterns, commands,
unknown configuration fields, and malformed variant catalogs.

## Consume the machine contract

`cargo rail plan --json` writes one schema-owned value.
The checked-in contract is [`plan-v9.schema.json`](../schemas/plan-v9.schema.json):

```bash
cargo rail plan --schema > target/plan.schema.json
cargo rail plan --json > target/plan.json
```

These fields have execution authority:

- `identity`: the content-derived decision identity;
- `inputs`: comparison, source, Cargo, configuration, toolchain, platform, catalog,
  and evidence bindings;
- `work`: every tagged required or skipped decision;
- `required`: the sorted projection of required work IDs; and
- `work.NAME.scope`: the selector that only required work has.

`changes`, causes, explanations, and evidence describe a decision.
They are not selectors.
Read `work.NAME.scope.selection.cargo_args` as an argument array, never as shell text.
Treat variant `kind = "all"` as an instruction to materialize the complete checked-in catalog of the owning
workflow.

Before each executor starts, do these steps:

1. Validate the complete plan and required-work projection.
1. Select one known required work item.
1. Lower only the typed scope of that item.
1. In the execution workspace, run the checkout verification of the matching reader.
1. Start the executor only after verification succeeds.

The companion GitHub Action owns the independent strict consumer.
It publishes the exact plan, reader, and Cargo-Rail version that were used to create the decision.
Every execution job installs that exact version
before the reader delegates checkout verification to Cargo-Rail.
A comparison of `HEAD` alone is not sufficient.
When drift exists, verification exits `2` before any selector is emitted or work starts.
A reader that already captured a plan uses `cargo rail plan --verify -`.
It passes those exact bytes on standard input, so verification cannot reopen a different pathname.

Verification binds the plan to the current platform, Cargo configuration, toolchain, workspace,
and source.
The `required` list of another platform can route a job.
However, only a plan created on the platform of the job passes verification there.
The checkout can move.
The same commit at another path passes verification.
Another workspace in the same repository does not pass.
Store a transferred plan outside the checkout.
An unignored plan file inside the checkout is untracked drift.

## Observed-input evidence

Without evidence, a changed file widens every built-in Cargo work item if a compiler, build script,
or procedural macro could read the file.
This applies even to a README.

A changed `rust-toolchain.toml`, `rust-toolchain`, or `.cargo/config.toml` at the workspace root requires every built-in Cargo item for the whole workspace.
These files have an effect on every package.

In a nested workspace, a changed file outside the workspace directory also counts.
A changed Cargo configuration or toolchain file in a parent directory of the workspace root requires
every built-in Cargo item for the whole workspace.
A changed file of one of these types in a parent directory requires the matching work item:

- `rustfmt.toml` or `.rustfmt.toml` requires `cargo.fmt`.
- `clippy.toml` or `.clippy.toml` requires `cargo.clippy`.
- `deny.toml` requires `dependency-policy`.

Any other outside file widens built-in Cargo work until evidence shows that no compiler read it.
`changes.files` lists only workspace paths.
The evidence input names outside files as `../` paths.

Record portable evidence from the ordinary build of the commit that later changes compare against:

```bash
cargo rail plan evidence --work cargo.test --output target/planning-evidence/cargo.test.json \
  -- test --workspace --locked --no-run
cargo rail plan --evidence target/planning-evidence/cargo.test.json --explain-work cargo.test
```

The command runs Cargo with a recorder as `RUSTC_WRAPPER`.
The recorder records each workspace compiler invocation and runs the compiler unchanged.
Thus outputs, Cargo freshness, and any configured wrapper, such as the compiler cache,
operate as usual.
After the build, Cargo-Rail reads the dep-info file that rustc wrote for every workspace unit.
It also reads the rerun declarations of every build script.
A unit that Cargo reports as fresh keeps the dep-info of its last compilation.
That dep-info is the input set that Cargo just checked.
Thus recording also works on a warm target directory.
Cargo-Rail binds each input to its Git object at `HEAD`.
If the build fails or a tracked file differs from `HEAD`, the command fails and writes no evidence.
Cargo diagnostics and test output go to standard error.

Record `cargo.build`, `cargo.clippy`, and `cargo.test`, with one work item in each run.
Use the packages, targets, features, and profile of the command that the job runs.
The evidence covers only what the recorded build compiled.
The planner applies the evidence to the whole work item.
Every workspace member must compile at least one unit.
Otherwise, the evidence is incomplete.
When the bindings match, a write to an existing file keeps the other work items in that file.
Documentation, doctest, and packaging work have no recorder.
These work items continue to widen.

The evidence follows the rebuild model of Cargo:

- A changed file selects every package whose units read the file.
  It also selects the packages whose builds depend on those packages.
  A changed integration-test input selects only that test target.
- The `rerun-if-changed` paths of a build script are inputs.
  A declared directory also covers files that are added to it.
  A script that declares no path depends on every file in its package.
- A file that a procedural macro reads counts only when the macro reports the read to the compiler,
  as Cargo requires.
  A macro that reads a file without a report can become stale under Cargo too.
- Any changed `clippy.toml` or `.clippy.toml` selects `cargo.clippy` for the whole workspace.
  A Clippy configuration file can govern many packages, and dep-info names only files that exist.
- Files that a test reads at run time, such as a fixture, are not compiler inputs.
  Declare them as repository work.

In these conditions, Cargo-Rail marks the work item incomplete,
and the planner continues to widen it:

- The dep-info of a unit is not available.
- A unit reads an untracked file that Git does not ignore.
- A unit reads a repository file outside the workspace.
- A build script declares a missing path or a directory with no tracked files.
- An input names a secret-like environment variable.

The command lists what it could not observe.

Pass one `--evidence` flag for each file.
[`planning-evidence-v2.schema.json`](../schemas/planning-evidence-v2.schema.json) binds each manifest to its source commit, Cargo universe,
Cargo configuration, toolchain, target, platform, provider capabilities, and work items.
Cargo-Rail validates each file separately.
A missing, stale, malformed, or foreign file, or two files for one work item,
widens only the work items that have no other compatible evidence.
The plan lists the identities of the manifests that it used.
`--explain-work` names the manifest behind each observed decision.

### Transfer evidence in CI

Record evidence in the jobs that already build on the default branch.
Save the file under a key for its commit:

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

The later test run finds every unit fresh.
Thus recording adds no compilation.
Before planning, restore the evidence of the comparison base.
The default pull-request checkout is a merge commit.
For that checkout, the base is the base commit of the pull request:

```yaml
- uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
  with:
    path: target/planning-evidence/cargo.test.json
    key: cargo-rail-evidence-${{ runner.os }}-${{ runner.arch }}-cargo.test-${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before }}
```

Repeat the save and restore steps for each recorded work item.
Pass the directory to the `evidence` input of the [companion Action](https://github.com/loadingalias/cargo-rail-action).
A cache miss, or evidence for a different base, only causes widening.
Make sure that Git ignores the directory.
Otherwise, an evidence file in it is a changed path.

A plan identity lets you compare decisions.
It is not a cache key.
It never authorizes the reuse of compiler results.

The current planner accepts variant catalog v2.
When you write a catalog, use the checked-in v2 schema.

## Diagnose a decision

```bash
cargo rail config validate --strict
cargo rail plan --explain
cargo rail plan --json | jq '{inputs, changes, required, work}'
```

If the compared states, selected work, or executor scope are not what you expect,
see [Troubleshooting](troubleshooting.md).

### Impact attribution

Plan contract v9 adds `attribution`.
Its keys are exactly the required work IDs.
It records the typed inputs that triggered the work.
It also records one relation for every selected package or variant: `direct`, `dependency`, or `unattributed`.
A dependency relation names one captured originating package.
It does not claim to list every causal path.
A package that changed directly can also have affected dependencies.
The presentation still shows that package as direct.

Cargo-Rail binds attribution into the canonical plan identity.
Attribution explains the final selectors.
It does not change their execution authority.
Workspace scope, incomplete evidence, and `--all` stay explicit.
Consumers must not reconstruct these relations from paths or from the human evidence description.
The Action keeps direct selections visible.
It puts dependency details in expandable sections.
CLI `--explain` includes the complete selected scope.

Cargo-Rail emits and verifies v9 plans.
Regenerate older saved plans.
Use a companion Action that independently validates v9 attribution.

## Check route parity before a migration

Before you replace path filters or another CI selector, record the routes that you expect.
Then compare them with the planner:

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

For each repository-relative `change` path, a case appends `append` to the file or creates the file.
The default `append` value is one newline.
Cargo-Rail writes the result as an unreferenced commit on top of `HEAD`.
It plans that commit in object mode.
The worktree, index, refs, and untracked files do not change.
They do not affect a case.
`required` fails when the work is skipped.
The failure names the missing work ID.
`skipped` fails when the work is required.
To fail when conservative expansion is the only reason that expected work is required, set `precise = true`.
The report lists every decision as `direct` or `expanded: incomplete evidence`.
It also lists the reasons for each expansion.
The command exits `1` when a case fails.
It exits `2` when the case file is invalid.
It has no JSON output.

A passing case is diagnostic.
It never becomes evidence that work can be skipped.
Without compatible portable evidence, an unrelated file can still widen Cargo work,
because compiler macros, build scripts, and procedural macros can read repository files.
To skip that work, record [observed-input evidence](#observed-input-evidence).
Do not use path patterns to avoid the expansion.

Each check proves one claim:

| Command                                   | Proves |
| ----------------------------------------- | ------ |
| `cargo rail config validate --strict`     | Policy is valid for the Cargo graph of this workspace. |
| `cargo rail plan --all --json`            | Every registered work item has valid full scope. It proves no routing decision. |
| `cargo rail plan --cases FILE`            | The reviewed path cases route as expected from `HEAD`. |
| `cargo rail plan --explain-work WORK_ID`  | Why one work item was required or skipped, including its paths and evidence. |
| `cargo rail plan evidence --work WORK_ID` | Which workspace files the recorded build read, bound to `HEAD`. |

When one job runs or skips unexpectedly, start with `--explain-work`.
