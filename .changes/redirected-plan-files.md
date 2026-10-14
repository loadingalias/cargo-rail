---
cargo-rail = "patch"
---

Apply a mutation plan that was saved inside the worktree.
Before, `config migrate -f json > migrate.json` followed by `config migrate apply --plan migrate.json` failed with "mutation drift detected".
This failure occurred because the shell created `migrate.json` before the preview captured the worktree.
The same failure occurred after a second `unify --check -o plan.json` run and with every other `--plan` consumer.
The preview now ignores the file that its standard output or `-o` writes.
Apply now ignores the file that `--plan` names.
A relative `--plan` or `-o` path is relative to the current directory.
