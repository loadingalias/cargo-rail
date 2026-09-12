---
"cargo-rail" = "minor"
---

Cargo-Rail now accepts one current contract per operation.
Automatic configuration translation, old release and acquisition journal readers,
split/sync mapping conversion, and `cache drop-unbound` are removed.

Breaking changes for v0.26.0:

- Author current configuration explicitly.
  For example, replace `unify.msrv = false` with `unify.msrv_policy = { mode = "disabled" }`, `release.push = false` with `release.remote_effects = "none"`, and split `paths` with Cargo member names in `members`.
  Omit `transitive_pinning` to disable it; use `transitive_pinning = { host = "root" }` to enable it.
  Inspect current fields with `cargo rail config explain --all` using supported input.
- Historical planning policy must also use current fields.
  Unsupported comparisons fail with the revision and path.
  Select a supported baseline only if it covers the intended work;
  otherwise perform explicit full verification outside affected planning
  while establishing a supported baseline.
  Moving the baseline can omit changes.
- Preserve unsupported installation receipts.
  Use the executable that created the installation to preview and perform its removal,
  then run current `cargo rail cache setup` in each workspace.
  If its originating release is unknown, determine that before removal;
  a schema number alone does not identify the executable.
- Preserve unsupported release journals, conflict receipts, and prepared Git effects.
  Finish or safely abort/reconcile them with their originating executable
  before starting conflicting work.
  Publication may already have occurred.
- Old split/sync mapping notes and trailers cannot continue through conversion.
  Continue the established relationship with its originating executable,
  or create a separate fresh target through an explicitly reviewed operation.
  Deleting notes does not make existing history a safe fresh target.
- Regenerate unsupported disposable plans and compiler evidence.
  Acquisition journal 3 uses a separate namespace;
  explicit resume refuses unsupported progress without rewriting it.
  Cache status 16, configuration explanation 2, release plan/state 8, sync conflict receipt 4,
  and prepared Git effect 2 require current consumers.
- Retired schema files are removed from the active branch and package: `plan-v8`, `plan-variants-v1`, `surface-v1`, `surface-v2`, and `config-explain-v1`.
  Historical tagged source remains available; URLs under `main` no longer serve those schemas.

Use `release check` instead of `release run --check`, and omit retired `--skip-publish`, `plan --merge-base`, and `change check --required` flags.
Current cache lifecycle operations are `setup`, `detach`, `drop-profile`, and `uninstall`.
Current configuration, recovery, ownership validation,
and authenticated installation remain supported.

Concurrent setup for separate workspaces no longer rejects another profile's exact completed
enrollment as transaction drift.
