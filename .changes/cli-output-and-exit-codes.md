---
cargo-rail = "patch"
---

Reject `--check` and other `unify` or `config migrate` options that come before the `apply` subcommand.
Before, `cargo rail unify --check apply` ignored `--check` and wrote manifests.

Keep machine output to one JSON value.
`--json unify undo` no longer prints `backup restored` before the JSON result.
When a `--diagnostics-file` write fails after a successful command, Cargo-Rail reports the failure on stderr.
A missing working directory or a failed standard-output write now causes exit code 2, not 1.
Exit code 1 means a pending mutation.

`config migrate apply` no longer deletes a default-only file that `--config` selected explicitly.
A default-only file contains only default values.
`init` refuses to write below the Cargo workspace root,
because configuration discovery never reads a file there.
`init --force` no longer overwrites a file that another discovery location shadows.

`config explain --all` lists `release.auxiliary_cargo_manifests` and `unify.transitive_pinning.host` when they are unset.
`config explain` also accepts these fields by name.
The `plan` help examples save and verify the plan under `target/`.
An untracked plan file inside the checkout is itself drift.

Without `--since`, `plan` and `change check` compare against the first of these refs that exists: `origin/HEAD`, `origin/main`, `origin/master`, `main`, or `master`.
They fail when none of these refs exists.
Before, they compared against `HEAD~1`, so the comparison left out earlier commits on the branch.
