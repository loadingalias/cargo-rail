# Release a workspace

`cargo rail release` releases every crate with release-worthy changes.
It shows one preview and asks for confirmation one time.
Then it commits the versions and changelogs, creates tags, pushes, publishes to the registry,
and creates GitHub releases.
After any interruption, run the same command again.
It finishes the release and does not repeat a completed step.

```bash
cargo rail release --dry-run   # preview and run every check; change nothing
cargo rail release             # release after one confirmation
cargo rail release --yes       # release without the prompt, as CI does
```

A release keeps no state of its own.
Git tags, the registry, and GitHub record what happened.
Each run reads these records.

## Before the first effect

A new release starts only when all of these conditions are true:

- the current branch exists on `origin`, and `HEAD` is the tip of that branch on `origin`;
- the worktree is clean, and no untracked files exist;
- the checks gate passes on `HEAD` (see [Checks](#checks));
- no planned tag exists on `origin` or locally;
- no planned version is already in the registry;
- the semver policy accepts the API change of each crate;
- every publishing crate packages and builds from the release commit
  without registry or forge tokens;
- each GitHub release has notes: the changelog section of the crate for the new version.

Any failure stops the command before it changes anything.
`--dry-run` runs all of these checks.
Thus a passing dry run means that the release can start.

## Select the crates

If you give no crate names, the release includes every crate that has release-worthy changes
since its last tag.
Change files, conventional commits, or both decide what is release-worthy (`release.source`).

```bash
cargo rail release my-lib --bump minor
```

Named crates add the published dependents and version-group members that they need.
That expansion skips members whose Cargo `package.publish` is `false`.
To include such a member, name it.
`release.crates` limits the crates that a release manages.
`change check` requires change files only for those crates.

`--bump` accepts `auto` (the default), `major`, `minor`, `patch`, `prerelease`, `release`, or an exact version.
When the manifests already contain the intended release version, use an exact version.
`auto` applies the reviewed bump to the current version.

## What a release does

Effects run in this order:

1. **Release commit.**
   Cargo-Rail writes the versions, lockfiles, changelogs, and consumed change files into one commit,
   `chore(release): my-lib v1.3.0`.
   The commit has a `Rail-Release-Crates: my-lib@1.3.0` trailer.
   Cargo-Rail creates the commit without a change to the worktree.
   Then it packages and builds each publishing crate from that commit.
1. **Tags and push.**
   The branch moves to the release commit.
   Cargo-Rail creates an annotated tag for each crate.
   With `sign_tags = true`, it signs each tag.
   One atomic push sends the branch and every tag.
   The push succeeds only when `origin` still has the branch tip that the release started from,
   and has none of the tags.
   If another push moved the branch, no change reaches `origin`,
   and the checkout returns to its starting commit.
1. **Assets.**
   For each crate with `release.assets`, Cargo-Rail creates a draft GitHub release.
   It stops with `awaiting_assets` until every listed file is attached.
   Then it downloads each file and compares it with the SHA-256 digest that GitHub recorded.
   It also verifies the attestation of each file (see [Release binaries](#release-binaries)).
1. **Registry.**
   Cargo-Rail uploads each publishing crate in dependency order from a temporary clone of the
   release commit.
1. **GitHub releases.**
   Cargo-Rail publishes the drafts.
   Each other crate gets a release named `my-lib v1.3.0`, with its changelog section as the notes.
   A pre-release version gets a pre-release.
1. **Aliases.**
   Each configured alias tag, such as `v{major}`, moves to the new release tag.
   An alias only moves forward along its release line.

The assets step comes before the registry step.
Thus a failed asset build leaves no published package.
You cannot undo a registry upload.
Only the steps before it can be repeated freely.

## Run again to finish

A rerun reads the most recent release on the branch and completes the missing steps:

| Interrupted after                 | The rerun |
| --------------------------------- | --------- |
| The local commit, before the push | Pushes the branch and tags. |
| The push                          | Publishes and creates releases. |
| One registry upload               | Uploads only the crates that the registry does not have. |
| The registry                      | Creates or publishes the GitHub releases. |
| Some assets                       | Waits for the remaining assets. |
| The GitHub releases               | Moves the aliases. |

The finish does not depend on the local worktree.
Cargo-Rail packages from a temporary clone of the tagged commit.
Run the command from any checkout of the release branch, even one that is behind `origin`.

The registry records the commit that each package came from (`.cargo_vcs_info.json`).
Before Cargo-Rail counts a published version as done,
it downloads the package and requires that commit to be the release commit.
A version that was published from another commit, or that was yanked, stops the release.
In that case, release a new version.

## Checks

On a GitHub `origin`, the default `release.checks` value is `"all"`.
Every check run and commit status that GitHub reports on the source commit must succeed.
`neutral` and `skipped` also pass.
At least one check run or commit status must exist.
The gate excludes the workflow run that invokes the release.
Thus a release job does not wait for itself.

| Checks on the source commit | Result |
| --------------------------- | ------ |
| All passed                  | The release continues. |
| Some still running          | `awaiting_checks`, exit code `1`, no effect. Run again when they finish. |
| Any failed                  | Error, exit code `2`, no effect. |
| None reported               | Error, exit code `2`. Wait for CI to start, or set `checks = "none"`. |

The release commit changes only versions, lockfiles, changelogs, and change files.
Thus it inherits the checks of its parent.
CI does not run again to authorize it.
For a repository without CI, set `checks = "none"`.
When `origin` is not on GitHub, the default is `"none"`.

## Protected branches

When the branch requires pull requests, open the release commit as a pull request:

```bash
cargo rail release --pr
```

Cargo-Rail pushes the release commit to `rail/release/<crate>-v<version>`.
Then it opens a pull request whose description contains the release trailer.
While the pull request is open, `cargo rail release` reports it and does nothing else.
After the merge, run `cargo rail release` again.
When the checks of the merged commit itself pass, the command tags that commit.
Then it publishes.

All merge methods work.
A merge commit or rebase keeps the trailer on the branch.
When a squash merge removes the trailer,
Cargo-Rail finds the merged release pull request through GitHub.
It reads the trailer from the head commit of the pull request.
If the branch moved before the merge,
the merged commit also contains changes that the release changelog does not describe.
In that case, the result names the pull request in a warning.
To prevent this, require branches to be up to date before a merge.

## Release binaries

List the release files of each crate in `release.assets`.
Names can use `{crate}` and `{version}`.

```toml
[release.assets]
my-tool = [
  "my-tool-{version}-x86_64-unknown-linux-gnu.tar.gz",
  "my-tool-{version}-aarch64-apple-darwin.tar.gz",
  "SHA256SUMS",
]
```

Build the files from the release tag in CI.
Attach them with [`loadingalias/cargo-rail-action/release/asset`](#ci).
That action attests each file and uploads it to the draft.
Before publication, Cargo-Rail runs `gh attestation verify` on every file.
The attestation must come from a workflow of this repository.
The run that made the attestation must have started at the release commit or at the commit
that the release was prepared from.
Verification needs the GitHub CLI 2.49 or later.

A private repository on a GitHub plan without artifact attestations can set `asset_attestations = "off"`.
Then each file must only be present and match its recorded digest.

## CI

The Action installs the locked Cargo-Rail release and runs the same `cargo rail release --yes`.
Pin the Action to a commit.

```yaml
name: Release
on:
  workflow_dispatch:
    inputs:
      bump:
        type: string
        default: auto
permissions: {}
concurrency:
  group: release
  cancel-in-progress: false
jobs:
  release:
    runs-on: ubuntu-latest
    environment: release
    permissions:
      contents: write
      actions: read
      checks: read
      pull-requests: read
      statuses: read
    steps:
      - uses: actions/checkout@<sha>
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: loadingalias/cargo-rail-action/release@<sha> # v12
        with:
          bump: ${{ inputs.bump }}
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

The Action gives its `token` input (default `github.token`) only to GitHub and to the release push.
The checkout keeps no credentials.
The Action commits as `github-actions[bot]` unless the checkout configures an identity.
Use crates.io trusted publishing to create `CARGO_REGISTRY_TOKEN` instead of a stored secret.
When branch rules block `github.token`, use a token that can push to the branch, or use `pull-request: true`.
The read permissions let the release read the checks of the source commit, this workflow run,
and release pull requests.
`pull-request: true` needs `pull-requests: write`.

The Action inputs are `crates`, `bump`, `pull-request`, `dry-run`, `token`, and `version`.
The Action outputs are `state`, `commit`, `tags` (a JSON array), and `pull-request`.
If a check is pending, the step fails.
For every other state, the step succeeds.

A binary release runs the step two times, with one asset job for each target between the two runs:

```yaml
  assets:
    needs: release
    if: needs.release.outputs.state == 'awaiting_assets'
    strategy:
      matrix:
        include:
          - { runner: ubuntu-latest, target: x86_64-unknown-linux-gnu }
          - { runner: macos-latest, target: aarch64-apple-darwin }
    runs-on: ${{ matrix.runner }}
    permissions:
      contents: write
      id-token: write
      attestations: write
      artifact-metadata: write
    steps:
      - uses: actions/checkout@<sha>
        with:
          ref: ${{ fromJSON(needs.release.outputs.tags)[0] }}
          persist-credentials: false
      - run: ./scripts/package.sh "${{ matrix.target }}" dist
      - uses: loadingalias/cargo-rail-action/release/asset@<sha> # v12
        with:
          tag: ${{ fromJSON(needs.release.outputs.tags)[0] }}
          files: dist/*

  publish:
    needs: [release, assets]
    if: needs.assets.result == 'success'
    # The same job as `release`; the second run verifies the assets, publishes, and finishes.
```

Give the `release` job an `outputs` map with `state: ${{ steps.<id>.outputs.state }}` and `tags: ${{ steps.<id>.outputs.tags }}`.

## Machine output

`cargo rail release --format json` prints one JSON value.
Its `state` is `nothing_to_release`, `planned`, `cancelled`, `pull_request`, `awaiting_checks`, `awaiting_assets`, or `released`.
`crates` lists the `version`, `tag`, `registry` state, and `github_release` state of each crate.
Each of the two states is `done`, `pending`, or `skipped`.
For a new release, each crate also has `notes` and `reason`.
While a draft waits, the crate also has `missing_assets`.
`commit`, `pull_request`, `pending_checks`, `next`, and `warnings` appear when they apply.
Exit code `0` means that the command did what the state says.
Exit code `1` means that the command waits for checks.
Exit code `2` means an error.

## Credentials

- **GitHub:** `GH_TOKEN` or `GITHUB_TOKEN`, otherwise `gh auth token`.
  The API root is `GITHUB_API_URL` (GitHub Enterprise Server sets it), otherwise `https://api.github.com`.
- **Registry:** Cargo's own credentials: `CARGO_REGISTRY_TOKEN` or `CARGO_REGISTRIES_<NAME>_TOKEN`, `cargo login`, or a credential provider.
  Before any effect, Cargo-Rail checks that a credential is configured.
  Cargo uses the credential only for the upload, which runs no build scripts.
  Packaging and building run without registry or forge tokens.

`release.publish` names `crates-io` (the default), `none`, or a registry declared in Cargo's `[registries]`.
The registry must serve a sparse index with public reads,
because Cargo-Rail reads published packages back.

## Projected Cargo files

Cargo-Rail projects the primary and declared auxiliary workspaces before the release commit.
An independent workspace can have a tracked lockfile that resolves a released crate by path.
Such a workspace must appear in `release.auxiliary_cargo_manifests`.
A release rejects an omission before any effect.
The release keeps compatible requirements of unreleased crates and path-only declarations.
It retargets incompatible simple local requirements.
It rejects ambiguous compound or inequality edits.
Cargo validates each projected lockfile with `metadata --locked`.

## Tags and API checks

The default tag format is `{crate}-v{version}`.
For example, version `1.2.3` of crate `my-crate` gets the tag `my-crate-v1.2.3`.
When an existing repository uses `v1.2.3` tags, set `tag_format = "v{version}"`.
A `[crates.NAME.release].tag_format` override gives one crate its own tag line.

With `semver_check = "deny"`, a published library without a matching previous tag has no available compatibility evidence.
That library blocks the release.
Cargo-Rail treats a release as a first release only when the registry shows no published versions.
The `warn` policy reports a missing baseline but does not block the release.
`off` skips the check.
When a GitHub changelog uses linked `/compare/` headings, generated headings keep that style.
They compare the previous tag with the new tag.

## Upgrade from Cargo-Rail 0.31

1. Run `cargo rail config migrate` and review the preview.
1. Run `cargo rail config migrate apply`.
   The migration replaces each retired `[release]` key with its successor.
   The successor keeps the behavior of the retired key.
   For example, a file without registry publication gets `publish = "none"`.
1. Replace the release workflow with the [Action example](#ci).
   The old `transaction`, `intent`, and `source` inputs no longer exist.
1. To finish a release that 0.31 started, run `cargo rail release`.
   The command recognizes a 0.31 release commit on the branch.
   It completes the tags, registry uploads, and GitHub releases of that commit.
1. When no release is in progress, delete the data that 0.31 left behind.
   The `cargo rail release` result names any `refs/notes/cargo-rail/*` refs on `origin`, with the command that removes them.
   `target/cargo-rail/releases` holds old local journals that nothing reads.

To adopt Cargo-Rail from release-plz or cargo-release, do these steps:

1. Finish any active release with that tool.
1. Review the tag formats and published versions.
1. Add change files for the next release.

Existing history and published versions stay inputs.
Nothing rewrites them.

## Cargo-Rail and its Action

Cargo-Rail publishes first.
Its [release workflow](../.github/workflows/release.yml) runs the engine built from source,
because the locked engine of the Action is older than the release that is in progress.
After that release, do these steps:

1. Update the Cargo-Rail lock of the Action to the exact version and commit.
1. Qualify the Action against that version.
1. Release the Action with its own workflow.
