---
cargo-rail = "patch"
---

Reject `--check` and other `unify` or `config migrate` options placed before the `apply` subcommand.
`cargo rail unify --check apply` previously ignored `--check` and wrote manifests.

Keep machine output to one JSON value: `--json unify undo` no longer prints `backup restored` before the JSON result,
and a `--diagnostics-file` write failure after a successful command is reported on stderr.
A missing working directory or a failed standard-output write now exits 2, not 1,
because exit 1 means a pending mutation.

`config migrate apply` keeps a default-only file that `--config` selected explicitly instead of deleting it.
`init` refuses to write below the Cargo workspace root, where configuration discovery would never read it,
and `init --force` no longer overwrites a file that another discovery location shadows.
`release check --publication` names `--publication`, not `--publish`, when release authority is missing.

`config explain --all` lists `release.auxiliary_cargo_manifests` and `unify.transitive_pinning.host` when they are unset, and `config explain` accepts them by name.
`release check` no longer says "Publication-ready changes detected" when no selected crate publishes.
The `plan` help saves and verifies the plan under `target/`;
an untracked plan file inside the checkout is itself drift.

`plan` and `change check` without `--since` compare against `origin/HEAD`, `origin/main`, `origin/master`, `main`, or `master`, in that order, and fail when none exists.
They previously compared against `HEAD~1`, which left earlier commits on the branch out of the comparison.
