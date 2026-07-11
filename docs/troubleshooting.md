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
2. Confirm the requested surface or profile.
3. Inspect confidence-profile expansion.
4. Inspect path classification and custom rules.
5. Check binary-only filtering when `--ignore-bin-crates` is set.

## Common mistakes

### Shallow CI checkout

`cargo rail release run --bump auto` needs release tags to measure each crate's
range. In a shallow clone, tags may be missing, so auto-bump planning fails
instead of guessing from incomplete history.

```bash
git fetch --unshallow --tags
```

In GitHub Actions, use `fetch-depth: 0` for release jobs.

### Interrupted release

`release run` and `release finalize` print a durable state path before their
first side effect. Resume that exact plan after any local, network, registry, or
forge failure:

```bash
cargo rail release resume target/cargo-rail/releases/release-<id>.json
```

Resume reconciles commits, tags, pushed refs, forge releases, and published
crate versions before advancing. It never replans from already-bumped manifests
or republishes an unobserved in-progress crate version.

If no remote, forge, or registry side effect has started, abandon the release
and restore its original local commit with:

```bash
cargo rail release abort target/cargo-rail/releases/release-<id>.json --yes
```

Abort refuses once an external side effect may exist; resume is the safe path
from that point.

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

### Custom surfaces in run profiles

Custom surfaces are planner outputs, not valid `run.profile.*.surfaces` entries.

```toml
# wrong
[run.profile.bench]
surfaces = ["custom:benchmarks"]

# right
[run.profile.bench]
surfaces = ["bench"]
```

### CI/local mismatch

Use the same base-ref strategy and profile in both places.

```bash
# local
cargo rail run --merge-base --profile ci

# CI
cargo rail run --since "$RAIL_SINCE" --profile ci
```
