# Troubleshooting

Start with the planner trace. It records the base ref, file classification, ownership, graph expansion, surface decisions, and execution scope.

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
- `scope.cargo_args`

Use `scope` for execution decisions. Use `impact` for diagnostic context.

## Why did this not run?

Check these in order:

1. Confirm the base ref contains the intended comparison range.
2. Confirm the requested action or profile.
3. Inspect confidence-profile expansion.
4. Inspect path classification and custom rules.
5. Check binary-only filtering when `--ignore-bin-crates` is set.

## Common mistakes

### Shallow CI checkout

Commit compatibility modes (`[release] source = "commits"` or `"both"`) need
release tags to measure each crate's history range. In a shallow clone, tags
may be missing, so commit-derived auto-bump planning fails instead of guessing
from incomplete history. The default reviewed-changes mode does not parse
commit history for bump selection.

```bash
git fetch --unshallow --tags
```

In GitHub Actions, use `fetch-depth: 0` for release jobs.

### Interrupted release

`release run` and `release finalize` print a durable state path before their
first side effect. Resume that exact plan after any local, network, registry, or
forge failure:

```bash
cargo rail release status
cargo rail release status --format json
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

Status does not load Cargo metadata, so it remains available if a crash left a
manifest between local mutations. It reports the phase, exact SHA, last and next
effect, persisted observations, ambiguity, recoverability, and one safe command.

Resume reconciles release commits, exact remote refs, check/pipeline rollups,
crate versions, tags, and forge releases before advancing. It never replans from
already-bumped manifests or republishes an unobserved in-progress crate version.
`awaiting_checks` and registry propagation are explicit external wait boundaries:
cargo-rail exits and never sleeps or polls. Resume after the provider settles.

Every release commit also records the plan-bound transaction plus release mode,
crate versions, and publish/tag authorization in `Rail-Release-*` trailers. In a
fresh checkout at the exact release commit, `release status` reconstructs the
transaction from Git and `cargo rail release resume release-<transaction-id>`
rebuilds the local journal before reconciling remote, forge, and registry truth.
Reconstruction refuses a different HEAD or legacy trailers that do not encode
irreversible-effect authorization.

If no remote, forge, or registry side effect has started, abandon the release
and restore its original local commit with:

```bash
cargo rail release abort target/cargo-rail/releases/release-<id>.json --yes
```

Abort refuses once an external side effect may exist; resume is the safe path
from that point.

Plain `cargo rail clean` removes completed and aborted release journals. It
refuses active or malformed journals before deleting anything; use `release
status` to resolve them first.

### Manual sync conflict

Manual sync conflicts exit with status `1`, leave the merged files on the
`cargo-rail-sync-<crate>` recovery branch, and print a receipt path. Resolve all
listed files, remove every conflict marker, then commit through the receipt:

```bash
cargo rail sync --resume target/cargo-rail/receipts/sync-conflict-<crate>-<id>.json
```

Resume verifies the branch, parent commit, owned paths, and marker-free content
before committing. Do not commit the conflict manually.

### Release PR finalization

`release finalize` expects the version bumps and changelog sections created by
`release run --pr` to already be present in the current checkout. Merge the
release PR first, update the target branch, then finalize from that merged
commit:

```bash
git switch main
git pull --ff-only
cargo rail release finalize --all --yes
```

### Planner surfaces versus run actions

Custom surfaces are planner outputs, not action IDs. Use them in a configured action's `when` policy, then select that
action from the profile.

```toml
# wrong
[run.profile.bench]
actions = ["custom:benchmarks"]

# right
[run.action.benchmark-report]
argv = ["cargo", "run", "-p", "xtask", "--", "benchmark-report"]
when = ["custom:benchmarks"]
inputs = ["benches"]

[run.action.benchmark-report.environment]
inherit = true

[run.profile.bench]
actions = ["bench", "benchmark-report"]
```

### CI/local mismatch

Use the same base-ref strategy and profile in both places.

```bash
# local
cargo rail run --merge-base --profile ci

# CI
cargo rail run --since "$RAIL_SINCE" --profile ci
```
