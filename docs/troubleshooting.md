# Troubleshooting

Use this when planner or runner behavior is not what you expected.

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

Use `scope` for execution decisions. Use `impact` for diagnostic context.

## Why did this not run?

Check these in order:

1. wrong base ref
2. wrong surface or profile
3. confidence profile behavior
4. path classification or custom rules
5. binary-only crate filtering via `--ignore-bin-crates`

## Common mistakes

### Shallow CI checkout

`cargo rail release run --bump auto` needs release tags to measure each crate's
range. In a shallow clone, tags may be missing, so auto-bump planning fails
instead of guessing from incomplete history.

```bash
git fetch --unshallow --tags
```

In GitHub Actions, use `fetch-depth: 0` for release jobs.

### Release failed after the release commit

`release run` applies local mutations before public side effects. If a failure
happens after the release commit is created but before publish completes, the
repository may already contain:

- bumped manifests,
- changelog sections for the target versions,
- consumed change files in the release commit,
- some local or pushed tags, depending on where the failure happened.

Re-run the same command first. cargo-rail checks existing versions and tags, so
an idempotent retry is usually the clean path after a transient registry,
network, or forge failure.

Revert only when the release should not happen. Reverting the release commit
brings consumed change files back because they are normal tracked files in that
commit. If tags were already pushed for a release you are abandoning, delete
those tags deliberately before opening a new release plan.

### Release PR finalize failed

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
