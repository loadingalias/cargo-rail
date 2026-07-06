# Release Example

Use `change` for reviewed release intent and `release` for checks, version
bumps, changelogs, tags, forge releases, and publish ordering.

## Quick Start

```bash
cargo rail change add my-crate --bump minor --message "Added the new capability."
cargo rail change status
cargo rail release check my-crate --extended
cargo rail release run my-crate --bump auto --check
cargo rail release run my-crate --bump auto --yes
git push origin main --follow-tags
```

`--bump auto` reads change files first, then conventional commits since each
crate's previous tag. Change files are consumed in the release commit;
consumption is all-or-nothing, so a plan covering only some of a file's
crates is rejected.

## Review-First Release

Use PR mode when the release diff needs review before any tag or publish:

```bash
cargo rail release run my-crate --bump auto --pr --yes
# merge the release PR, then run from the updated main branch:
cargo rail release finalize my-crate --yes
```

`release run --pr` only writes the version, lockfile, changelog, and consumed
change files to a release branch. `release finalize` tags, pushes, publishes,
and creates configured forge releases from the merged commit.

## Lockstep Crates

Use `[release.version_groups]` when crates must ship together:

```toml
[release.version_groups]
core = ["rail-core", "rail-graph", "rail-git"]
```

With `--bump auto`, the group releases at the highest bump any member earned.
Explicit partial group releases are rejected unless `--include-dependents`
expands the selection to the whole group.

## Reference

- [Configuration Reference](../../docs/config.md)
- [Migrate from git-cliff / release-plz](../../docs/migrate-git-cliff.md)
- [Architecture](../../docs/architecture.md)
