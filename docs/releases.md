# Release a workspace

Use `cargo rail release check --all` to review the selected packages, versions, notes, and effects.
Pending changes produce exit code `1`.
Checking performs no release effects.
Add `--extended` to run package publish dry-runs, MSRV checks, and configured semver checks locally.
`--extended` does not require `--publication`; the latter separately validates publication authority for the same plan.

```bash
cargo rail release run --all --publish
cargo rail release status
cargo rail release resume
```

Publication requires both `--publish` and `release.registry_publication = "crates-io"`.
Omit them for a Git-only release.
Use an exact bump, such as `--bump 1.2.3`, when the intended version is already written in the manifest;
`auto` applies the reviewed bump to the current version.
Commit all inputs before submitting a hosted request.

## Select the release

| Workspace                                | Request |
| ---------------------------------------- | ------- |
| Published library                        | `cargo rail release run my-lib --publish` |
| Mixed workspace                          | `cargo rail release run my-lib --include-dependents --publish` |
| Binary distributed through GitHub assets | `cargo rail release run my-tool` with `remote_effects = "github"` and an artifact inventory |
| Protected branch                         | `cargo rail release run --all --pr --publish` |

The selected package set and dependent closure belong to the original intent.
Review the check output before requesting publication.
Omit `--publish` when the release only creates Git and forge objects.

When adopting from release-plz or cargo-release, finish any active release with its original tool.
Replace its release job with the configured Cargo-Rail workflow,
review tag formats and published versions, and add change files for the next release.
Existing history and published versions remain inputs; migration does not rewrite them.
Use an explicit bump when the manifests already contain the intended release version.

The default tag format is `{crate}-{prefix}{version}` for every repository shape.
With the default `tag_prefix = "v"`, package `my-crate` version `1.2.3` receives `my-crate-v1.2.3`.
Set `tag_format = "{prefix}{version}"` explicitly when an existing single-crate repository uses `v1.2.3` tags.
When an existing GitHub changelog uses linked `/compare/` headings,
generated headings preserve that style and compare the previous tag with the planned tag.

## Choose execution

Without `release.hosted_workflow`, the command executes locally.
If checks are still pending, it retains the original transaction and reports the next action.
`resume` continues that transaction.

With a configured GitHub workflow, `run` retains the request remotely, dispatches that workflow,
and returns the transaction ID and run URL.
The job survives terminal closure.
`resume` submits continuation of the same request.
A dispatch failure never starts local publication.
`--local` explicitly selects local execution for a new request.

```toml
[release]
remote_effects = "github"
registry_publication = "crates-io"
hosted_workflow = ".github/workflows/release.yml"
validation = { ".github/workflows/ci.yml" = ["tests"] }
```

The validation workflow must accept `workflow_dispatch` and check out its event's exact commit.
Use the jobs' displayed names in `validation`.
The release workflow must be a separate workflow.
Cargo-Rail explicitly dispatches validation after pushing preparation,
checks the returned run's commit, and retains its run and attempt.
It does not depend on a token-authenticated push triggering CI.
Validation dispatch stops if the branch moves before its run is bound.
Later commits do not retarget already bound validation or artifacts.

GitHub is the hosted provider.
Local execution retains GitHub and GitLab publication support.
Alternate Cargo registries are not supported.

## Configure the GitHub Action

Create a caller-owned workflow with `workflow_dispatch` inputs named `transaction`, `intent`, and `source`, all optional strings.
Empty transaction starts a request; Cargo-Rail supplies all three values for continuation.
Expose `bump`, `publish`, and `review` inputs if operators need those choices.
Run the release job only from its authorized base branch.

The job needs `contents: write`, `actions: write`, and `pull-requests: write` for review mode.
Serialize it with a repository-wide concurrency group and `cancel-in-progress: false`.
Use an environment when publication requires approval.
Check out full Git history and provide Git push credentials, a Git commit identity, `gh`, Cargo,
and the configured signing authority.
Keep registry credentials in this job, outside validation and packaging jobs.

Invoke `loadingalias/cargo-rail-action/release` at a reviewed immutable Action commit.
Its `version` input defaults to the exact Cargo-Rail release in the Action's lock.
Set a different exact stable version only when the workflow requires another compatible engine.
The other inputs are `packages` (a JSON array; `[]` selects all), `bump`, `publish`, and `review`.
It exposes `transaction-id`, `release-sha`, `state`, and `run-url`.
The Action installs the selected authenticated Cargo-Rail components,
validates the record and invocation independently, and calls the core engine.
It does not choose package bumps or publish by itself.

For reviewed releases, also accept `pull_request_target: { types: [closed] }` and invoke the same Action for a merged,
same-repository `rail/release-` branch.
It verifies the recorded PR and prepared commit.
The merge must preserve the complete prepared tree.
Validation, packaging, and publication then run on
that exact merged commit under the original intent.
If the merge-event runner is lost,
`resume` rediscovers the retained PR merge and applies the same tree checks.

The Cargo-Rail repository [release workflow](../.github/workflows/release.yml) demonstrates direct use of the source-built engine
for its own bootstrap.
Cargo-Rail Action uses one locked Cargo-Rail release and source commit for its package
and publication workflows.
Neither repository resolves a moving tool version while publishing.

## Recover and inspect

`status` and `resume` discover retained remote records, including from a fresh clone.
Supply a transaction ID when local records contain several active transactions.
The record and original Cargo archives travel together through leased Git refs.
Recovery does not need the original runner.
Native artifacts remain in the producer's artifact storage and must stay available until completion.

The record exposes exact validation, artifact digests, upload attempts, tag objects,
forge release IDs, and alias progress.
Missing evidence or conflicting external objects stop continuation.
An upload with an uncertain registry result is never blindly repeated.
Publication cannot be rolled back.
`abort` is available only before a remote or registry effect may exist.

See [release records](release-records.md) for the wire contract and [release configuration](config.md#release-policy) for artifact inventories
and other policy.
Finish an older active transaction using its originating executable before adopting this contract;
there is no legacy record translation.

## Coordinate Cargo-Rail and Action releases

Qualify the source-built Cargo-Rail binary
and authenticated archive against the Action's independent consumer before publication.
Publish Cargo-Rail first, then verify its immutable release and executable attestations.
Update the Action repository's Cargo-Rail lock with that exact version
and dereferenced release commit.
If Cargo-Rail changes the release-record contract,
update and qualify the Action's independent schema and reader before moving the lock;
an older reader rejects the new record version.
The source-built Cargo-Rail bootstrap can publish core before that consumer transition.
The Action's release workflow validates the locked authenticated components and releases the Action.
Its configured `v10` alias moves only after the immutable release is verified and only
if the prior alias object still matches the request.

Local tests do not qualify Linux or Windows runtime behavior.
Complete the native runner lanes before publishing either release.
The workflows require their configured validation and packaging evidence.
