---
cargo-rail = "major"
---

Replace the release subcommands with one stateless command, `cargo rail release`.
This release breaks release commands, `[release]` configuration, and release workflows.
After you upgrade, run `cargo rail config migrate` and replace the release workflow.

- `cargo rail release [CRATE...] [--bump B] [--pr] [--yes] [--dry-run]` shows one preview and asks for confirmation once.
  Then it writes the release commit and tags it.
  It pushes the branch and every tag in one atomic push.
  It uploads to the registry in dependency order and creates GitHub releases.
  The push succeeds only if `origin` still has the starting branch tip and has none of the tags.
  `--dry-run` runs every precondition check and changes nothing.
- When you run the command again after an interruption, it finishes the release.
  It reads progress from the remote tags, the registry, and GitHub.
  It counts a published version as done only when the `.cargo_vcs_info.json` of that version names the release commit.
- This release removes `release run`, `check`, `resume`, `status`, `abort`, `record`, and `init`.
  Each removed subcommand prints its replacement.
  This release also removes hosted execution, `--local`, `--executor`, `--retain-remote`, `--prepare`, release plan files, `--allow-non-default-branch`, GitLab releases,
  and the MSRV check.
- Releases keep no records.
  This release removes release record refs, local release journals, `clean --release-journal`, and the `release-record-v11` schema.
  If `origin` still holds `refs/notes/cargo-rail/*` refs from 0.31 or earlier, the result shows a warning.
  The warning names these refs and gives the command that deletes them.
- A release requires that every check run and commit status on its source commit passes
  (`release.checks = "all"`, the default on a GitHub `origin`).
  Pending checks stop the command with exit code 1 before any effect.
- For GitHub access, Cargo-Rail uses the REST API with `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`.
  The GitHub CLI is not required, except to verify asset attestations.
- `[release]` adds `crates`, `publish`, `github_release`, `checks`, `assets`, and `asset_attestations`.
  It retires `remote_effects`, `registry_publication`, `hosted_workflow`, `validation`, `artifacts`, `tag_prefix`, `require_change_files`, `release_notes_dir`, `require_release_notes`, and the `{prefix}` tag placeholder.
  `config migrate` replaces the retired fields and keeps their behavior.
  It writes successors only to a file that contains a retired field or the `{prefix}` placeholder.
  For example, such a file without `registry_publication = "crates-io"` gets `publish = "none"`.
  Aliases accept `{major}` and `{minor}`.
- A configuration without retired fields gets the new release defaults.
  By default, 0.31 published to no registry and created no GitHub releases.
  By default, 0.32 publishes to crates.io.
  On a GitHub `origin`, it also creates GitHub releases and uses `checks = "all"`.
  To keep publication and GitHub releases off, set `publish = "none"`, `github_release = false`, and `checks = "none"`.
  Publication still needs a registry credential, and the release checks for it before any effect.
- `release.publish` names `crates-io`, `none`, or any Cargo registry with a sparse index.
  The registry credential check reads Cargo configuration.
  Packaging and semver checks run without registry or forge tokens.
- `release.crates` limits the crates that a release manages.
  `change check` requires change files only for these crates.
- `--pr` opens the release commit as a pull request.
  When a squash merge drops the release trailer,
  the next run finds the merged pull request through GitHub and tags the merged commit.
  If the branch moved before the merge, the run shows a warning.
- Before Cargo-Rail publishes a draft release,
  it downloads each `release.assets` file and compares the file with the SHA-256 that GitHub recorded.
  Then it runs `gh attestation verify` for this repository and the release commit or its parent.
  `release.asset_attestations = "off"` skips the attestation check.
- The command finishes a release commit that Cargo-Rail 0.31 created.
- If a literal `vN` alias would name a different major version, the release stops before any effect.
