# Release a workspace

`cargo rail release` releases every crate with release-worthy changes.
It shows one preview, asks once, then commits the versions and changelogs, tags, pushes,
publishes to the registry, and creates GitHub releases.
Run the same command again after any interruption;
it finishes the release without repeating a completed step.

```bash
cargo rail release --dry-run   # preview and run every check; change nothing
cargo rail release             # release after one confirmation
cargo rail release --yes       # release without the prompt, as CI does
```

A release keeps no state of its own.
Git tags, the registry, and GitHub record what happened, and each run reads them.

## Before the first effect

A new release starts only when all of these hold:

- the current branch exists on `origin`, and `HEAD` is `origin`'s tip of that branch;
- the worktree is clean, including untracked files;
- the checks gate passes on `HEAD` (see [Checks](#checks));
- no planned tag exists on `origin` or locally;
- no planned version is already in the registry;
- the semver policy accepts each crate's API change;
- every crate that publishes packages and builds from the release commit,
  without registry or forge tokens;
- each GitHub release has notes: the crate's changelog section for the new version.

Any failure stops the command before it changes anything.
`--dry-run` runs every one of these checks, so a passing dry run means the release can start.

## Select the crates

With no crate names, the release includes every crate with release-worthy changes
since its last tag.
Change files, conventional commits, or both decide what is release-worthy (`release.source`).

```bash
cargo rail release my-lib --bump minor
```

Named crates add the published dependents and version-group members they need.
That expansion skips members whose Cargo `package.publish` is `false`; name them to include them.
`release.crates` limits the crates a release manages; `change check` requires change files only for those crates.

`--bump` accepts `auto` (the default), `major`, `minor`, `patch`, `prerelease`, `release`, or an exact version.
Use an exact version when the manifests already contain the intended release version;
`auto` applies the reviewed bump to the current version.

## What a release does

Effects run in this order:

1. **Release commit.**
   Cargo-Rail writes the versions, lockfiles, changelogs, and consumed change files into one commit,
   `chore(release): my-lib v1.3.0`, with a `Rail-Release-Crates: my-lib@1.3.0` trailer.
   It builds the commit without touching the worktree,
   then packages and builds each publishing crate from it.
1. **Tags and push.**
   The branch moves to the release commit, an annotated tag is created for each crate
   (signed with `sign_tags = true`), and one atomic push sends the branch and every tag.
   The push succeeds only when `origin` still has the branch tip the release started from
   and none of the tags.
   If another push moved the branch, nothing lands, and the checkout returns to its starting commit.
1. **Assets.**
   For each crate with `release.assets`, Cargo-Rail creates a draft GitHub release and stops with `awaiting_assets`
   until every listed file is attached.
   It then downloads each file, compares it with the SHA-256 GitHub recorded,
   and verifies its attestation (see [Release binaries](#release-binaries)).
1. **Registry.**
   Each publishing crate is uploaded in dependency order from a temporary clone of the release
   commit.
1. **GitHub releases.**
   Drafts are published; other crates get a release named `my-lib v1.3.0` whose notes are its changelog section.
   A pre-release version gets a pre-release.
1. **Aliases.**
   Each configured alias tag, such as `v{major}`, moves to the new release tag.
   An alias only moves forward along its release line.

Assets come before the registry, so a failed asset build leaves no published package.
A registry upload cannot be undone; only the steps before it can be repeated freely.

## Run again to finish

A rerun reads the most recent release on the branch and completes whatever is missing:

| Interrupted after                 | The rerun |
| --------------------------------- | --------- |
| The local commit, before the push | Pushes the branch and tags. |
| The push                          | Publishes and creates releases. |
| One registry upload               | Uploads only the crates the registry lacks. |
| The registry                      | Creates or publishes the GitHub releases. |
| Some assets                       | Waits for the rest. |
| The GitHub releases               | Moves the aliases. |

Finishing does not depend on the local worktree:
Cargo-Rail packages from a temporary clone of the tagged commit.
Run it from any checkout of the release branch, even one behind `origin`.

The registry records the commit each package came from (`.cargo_vcs_info.json`).
Before it counts a published version as done,
Cargo-Rail downloads the package and requires that commit to be the release commit.
A version published from another commit, or yanked, stops the release.
Release a new version in that case.

## Checks

On a GitHub `origin`, `release.checks` defaults to `"all"`: every check run
and commit status GitHub reports on the source commit must succeed
(`neutral` and `skipped` also pass), and at least one must exist.
The workflow run that invokes the release is excluded, so a release job does not wait on itself.

| Checks on the source commit | Result |
| --------------------------- | ------ |
| All passed                  | The release proceeds. |
| Some still running          | `awaiting_checks`, exit code `1`, no effect. Run again when they finish. |
| Any failed                  | Error, exit code `2`, no effect. |
| None reported               | Error, exit code `2`. Wait for CI to start, or set `checks = "none"`. |

The release commit changes only versions, lockfiles, changelogs, and change files,
so it inherits its parent's checks; CI does not run again to authorize it.
Set `checks = "none"` for a repository without CI.
Off GitHub, the default is `"none"`.

## Protected branches

When the branch requires pull requests, open the release commit as a pull request:

```bash
cargo rail release --pr
```

Cargo-Rail pushes the release commit to `rail/release/<crate>-v<version>`
and opens a pull request whose description carries the release trailer.
While it is open, `cargo rail release` reports it and does nothing else.
After the merge, run `cargo rail release` again.
It tags the merged commit once that commit's own checks pass, then publishes.

Every merge method works.
A merge commit or rebase keeps the trailer on the branch.
When a squash merge drops it, Cargo-Rail finds the merged release pull request through GitHub
and reads the trailer from the pull request's head commit.
If the branch moved before the merge,
the merged commit also contains changes the release changelog does not describe;
the result names the pull request in a warning.
Require branches to be up to date before merging to prevent that.

## Release binaries

List each crate's release files in `release.assets`.
Names may use `{crate}` and `{version}`.

```toml
[release.assets]
my-tool = [
  "my-tool-{version}-x86_64-unknown-linux-gnu.tar.gz",
  "my-tool-{version}-aarch64-apple-darwin.tar.gz",
  "SHA256SUMS",
]
```

Build the files from the release tag in CI
and attach them with [`loadingalias/cargo-rail-action/release/asset`](#ci),
which attests each file and uploads it to the draft.
Before publishing, Cargo-Rail runs `gh attestation verify` on every file.
The attestation must come from this repository's workflows,
made in a run that started at the release commit or at the commit the release was prepared from.
Verification needs the GitHub CLI 2.49 or later.

Private repositories on GitHub plans without artifact attestations can set `asset_attestations = "off"`;
files must then only be present and match their recorded digest.

## CI

The Action installs the locked Cargo-Rail release and runs the same `cargo rail release --yes`.
Pin it by commit.

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

The Action passes its `token` input (default `github.token`) to GitHub and to the release push alone;
the checkout keeps no credentials.
It commits as `github-actions[bot]` unless the checkout configures an identity.
Use crates.io trusted publishing to mint `CARGO_REGISTRY_TOKEN` instead of storing a secret.
Use a token that may push to the branch when branch rules block `github.token`, or use `pull-request: true`.

Its inputs are `crates`, `bump`, `pull-request`, `dry-run`, `token`, and `version`.
Its outputs are `state`, `commit`, `tags` (a JSON array), and `pull-request`.
A pending check fails the step; every other state succeeds.

A binary release runs the step twice, around one asset job per target:

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
`crates` lists each crate's `version`, `tag`, and `registry` and `github_release` state
(`done`, `pending`, or `skipped`), with `notes` and `reason` for a new release and `missing_assets` while a draft waits.
`commit`, `pull_request`, `pending_checks`, `next`, and `warnings` appear when they apply.
Exit code `0` means the command did what the state says, `1` means it waits for checks,
and `2` means an error.

## Credentials

- **GitHub:** `GH_TOKEN` or `GITHUB_TOKEN`, otherwise `gh auth token`.
  The API root is `GITHUB_API_URL` (GitHub Enterprise Server sets it), otherwise `https://api.github.com`.
- **Registry:** Cargo's own credentials: `CARGO_REGISTRY_TOKEN` or `CARGO_REGISTRIES_<NAME>_TOKEN`, `cargo login`, or a credential provider.
  Cargo-Rail checks one is configured before any effect; Cargo uses it only for the upload,
  which runs no build scripts.
  Packaging and building run without registry or forge tokens.

`release.publish` names `crates-io` (the default), `none`, or a registry declared in Cargo's `[registries]`.
The registry must serve a sparse index with public reads,
because Cargo-Rail reads published packages back.

## Projected Cargo files

Cargo-Rail projects the primary and declared auxiliary workspaces before the release commit.
An independent workspace with a tracked lockfile
that resolves a released crate by path must appear in `release.auxiliary_cargo_manifests`;
a release rejects omissions before any effect.
It preserves compatible requirements of unreleased crates and path-only declarations,
retargets incompatible simple local requirements,
and rejects ambiguous compound or inequality edits.
Cargo validates each projected lockfile with `metadata --locked`.

## Tags and API checks

The default tag format is `{crate}-v{version}`: crate `my-crate` version `1.2.3` gets `my-crate-v1.2.3`.
Set `tag_format = "v{version}"` when an existing repository uses `v1.2.3` tags.
A `[crates.NAME.release].tag_format` override gives one crate its own tag line.

With `semver_check = "deny"`, a published library without a matching previous tag has unavailable compatibility evidence
and blocks the release.
A first release is established only when the registry shows no published versions.
The `warn` policy reports a missing baseline without blocking; `off` skips the check.
When a GitHub changelog uses linked `/compare/` headings,
generated headings keep that style and compare the previous tag with the new one.

## Upgrade from Cargo-Rail 0.31

1. Run `cargo rail config migrate`, review the preview, then `cargo rail config migrate apply`.
   It replaces each retired `[release]` key with its successor and keeps what it did: for example,
   a file without registry publication gets `publish = "none"`.
1. Replace the release workflow with the [Action example](#ci); the old `transaction`, `intent`, and `source` inputs are gone.
1. Finish a release that 0.31 started by running `cargo rail release`;
   it recognizes a 0.31 release commit on the branch and completes its tags, registry uploads,
   and GitHub releases.
1. Delete what 0.31 left behind once no release is in flight:
   the result names any `refs/notes/cargo-rail/*` refs on `origin` with the command that removes them,
   and `target/cargo-rail/releases` holds old local journals nothing reads.

When adopting from release-plz or cargo-release, finish any active release with that tool,
review tag formats and published versions, and add change files for the next release.
Existing history and published versions remain inputs; nothing rewrites them.

## Cargo-Rail and its Action

Cargo-Rail publishes first: its [release workflow](../.github/workflows/release.yml) runs the source-built engine,
because the Action's locked engine predates the release being made.
After that release, update the Action's Cargo-Rail lock to the exact version and commit,
qualify the Action against it, and release the Action with its own workflow.
