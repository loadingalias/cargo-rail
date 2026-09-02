# Migrate from v0.25 to v0.26

v0.26 is a bounded compatibility break. It accepts the exact v0.25 repository configuration only through the
migration and historical-planning paths; current configuration, CLI, library, cache, and machine contracts reject
retired compatibility inputs.

## Migrate repository policy first

Run the read-only preview with the v0.26 binary:

```bash
cargo rail config migrate --check
cargo rail config validate --strict
cargo rail config explain --all
```

`config migrate --check` exits `1` when normalization is pending. Review every reported removal or replacement, then
run `cargo rail config migrate`. Apply mode preserves unrelated TOML and comments, revalidates immediately before
replacement, and reports the previous-file recovery path on platforms where that recovery file can be retained.

The frozen v0.25 migration performs these typed conversions:

- `unify.pin_transitives` plus `unify.transitive_host` becomes `unify.transitive_pinning`;
- `unify.msrv`, `unify.msrv_source`, and `unify.enforce_msrv_inheritance` become `unify.msrv_policy`;
- `release.push`, `release.create_github_release`, and `release.forge` become `release.remote_effects`; and
- `crates.<name>.split.paths` becomes package-name-based `members` after resolving each captured `Cargo.toml`.

It removes implementation switches whose behavior is now fixed, including Unify diagnostic/mutation toggles,
release cleanliness and changelog-shape controls, reserved `workspace`/`toolchain` tables, and reserved per-crate
`sync` tables. Reviewed `.changes` files are the only current release-note authority. Unknown fields and ambiguous or
invalid predecessor combinations fail without writing.

## Update command invocations

- Replace `cargo rail plan --merge-base` with `cargo rail plan`; merge-base comparison is the default.
- Replace `cargo rail release run --check` with `cargo rail release check`.
- Remove `--skip-publish`. Publication remains denied unless the exact invocation supplies `--publish` and repository
  policy permits crates.io publication.
- Remove `cargo rail change check --required`; current change coverage is fixed by reviewed release intent.
- Replace `cargo rail cache remove` with the operation you actually mean: `cache detach` for one workspace binding or
  `cache uninstall` for the global wrapper. Inspect `cache profiles`; remove only detached profiles with
  `cache drop-profile --profile PROFILE_ID`.

`release run --wait` is new. It stays attached to the same durable transaction while exact-SHA checks settle; an
interrupted wait remains resumable from the journal printed by Cargo-Rail.

## Re-enroll compiler-cache state

v0.26 installs one transparent wrapper per Cargo home but gives each enrolled workspace its own profile, trust domain,
remote authority, lifecycle state, and local CAS. A v0.25 machine-global receipt cannot prove which workspace owned
its remote selection, so it becomes unbound migration state and is never selected at runtime.

```bash
cargo rail cache profiles --json
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache drop-unbound --check
```

Enroll the intended workspace before removing retained unbound data. Cache status machine output is schema 15 and
includes the selected profile and workspace binding; consumers of schema 14 must update explicitly.

## Update planner catalogs and consumers

The current planner executes variant catalog v2 only. The v1 schema stays at its published path for stored-artifact
validation, but active catalogs must use typed `cargo_roots`. A root can now add:

- `features`: an exact no-default-feature root set; and
- `manifest`: an exact path registered by `release.auxiliary_cargo_manifests`.

Cargo-Rail narrows only captured source paths proved reachable through Cargo feature edges and ordinary Rust module
cfgs. Manifest, lockfile, toolchain, catalog, shared, or unattributed changes continue to select every row.

Object-bound CI plans can replace a host-specific Cargo-Rail verification binary with the v1 portable plan bundle.
The bundle contains `plan-v8.json`, `plan-read.py`, and `plan-bundle-v1.json`; the manifest binds the producer,
contract, platform limits, roles, sizes, and hashes. Run `verify-bundle` with the bundled reader before reading any
selector. Worktree plans still require matching Cargo-Rail verification.

## Review other contract changes

- GNU Linux archives require glibc 2.35 or newer; installers reject an unsupported host before replacement.
- `unify.compiler_targets` may select the exact subset of top-level resolution targets valid for workspace-wide
  compiler evidence. It never narrows dependency resolution.
- Surface analyzes the host by default; use `surface.targets = "workspace"` or an explicit non-empty list for more
  target domains.
- Public Rust types and command helpers that existed only for retired v0.25 inputs are removed. Downstream Rust users
  must compile against the current documented exports rather than compatibility types.

Historical plan, release-journal, cache, mapping, split/sync, and receipt schemas remain available at their published
paths. Their readers validate the frozen predecessor contract and either migrate through the bounded path above or
fail closed; current writers never emit the retired shapes.
