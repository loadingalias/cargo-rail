---
cargo-rail = "major"
---

Replace the release subcommands with one stateless command, `cargo rail release`.
This release breaks release commands, `[release]` configuration, and release workflows;
run `cargo rail config migrate` and replace the release workflow before upgrading.

- `cargo rail release [CRATE...] [--bump B] [--pr] [--yes] [--dry-run]` shows one preview and asks once.
  It then writes the release commit, tags it, pushes the branch and every tag in one atomic push,
  uploads to the registry in dependency order, and creates GitHub releases.
  The push succeeds only while `origin` still has the starting branch tip and none of the tags.
  `--dry-run` runs every precondition check and changes nothing.
- Running the command again after an interruption finishes the release.
  It reads progress from the remote tags, the registry, and GitHub,
  and it counts a published version as done only when its `.cargo_vcs_info.json` names the release commit.
- `release run`, `check`, `resume`, `status`, `abort`, `record`, and `init` are removed and print their replacement.
  Hosted execution, `--local`, `--executor`, `--retain-remote`, `--prepare`, release plan files, `--allow-non-default-branch`, GitLab releases, and the MSRV check are removed.
- Releases keep no records.
  Release record refs, local release journals, `clean --release-journal`, and the `release-record-v11` schema are removed.
  When `origin` still holds `refs/notes/cargo-rail/*` refs from 0.31 or earlier,
  the result names them in a warning with the command that deletes them.
- A release requires every check run and commit status on its source commit to pass
  (`release.checks = "all"`, the default on a GitHub `origin`).
  Pending checks stop the command before any effect with exit code 1.
- GitHub access uses the REST API with `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`.
  The GitHub CLI is needed only to verify asset attestations.
- `[release]` adds `crates`, `publish`, `github_release`, `checks`, `assets`, and `asset_attestations`.
  It retires `remote_effects`, `registry_publication`, `hosted_workflow`, `validation`, `artifacts`, `tag_prefix`, `require_change_files`, `release_notes_dir`, `require_release_notes`, and the `{prefix}` tag placeholder.
  `config migrate` replaces them and keeps their behavior; for example, a file without registry publication gets `publish = "none"`.
  Aliases accept `{major}` and `{minor}`.
- `release.publish` names `crates-io`, `none`, or any Cargo registry with a sparse index.
  The registry credential check reads Cargo configuration.
  Packaging and semver checks run without registry or forge tokens.
- `release.crates` limits the crates a release manages, and `change check` requires change files only for them.
- `--pr` lands the release commit through a pull request.
  When a squash merge drops the release trailer,
  the next run finds the merged pull request through GitHub and tags the merged commit;
  it warns when the branch moved before the merge.
- Before it publishes a draft release, Cargo-Rail downloads each `release.assets` file,
  compares it with the SHA-256 that GitHub recorded,
  and runs `gh attestation verify` for this repository and the release commit or its parent.
  `release.asset_attestations = "off"` skips the attestation check.
- The command finishes a release commit that Cargo-Rail 0.31 created.
- A literal `vN` alias that would name another major version stops the release before any effect.
