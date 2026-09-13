# Release execution records

Cargo-Rail retains the original release intent and execution progress
so another executor can recover the same release.
The [record schema](../schemas/release-record-v9.schema.json) owns the portable JSON shape.
Records contain no credentials or absolute runner paths.

## Identity and strict reading

The immutable intent binds the transaction ID, repository, source commit and tree,
selected packages, exact mutations and presentation, configuration, and authorized effects.
Its identity is `sha256:` followed by the lowercase SHA-256 digest of:

1. The UTF-8 bytes `cargo-rail-release-intent-v1`, followed by one NUL byte.
1. Compact UTF-8 JSON with the keys `intent` and `transaction_id`.
   Remove `intent.identity` before encoding.
   Sort object keys recursively and preserve array order.

Numbers must be integers.
Readers reject duplicate fields, unknown fields, omitted serialized defaults, unsafe paths,
inconsistent selections, and records larger than 16 MiB.
The schema alone does not prove identity, checkout binding, or invocation authority.
The companion Action validates the shared contract independently before exposing record outputs.

A saved intent, package seal, verified workflow attempt, or completed effect cannot be replaced.
Annotated tag objects retain their complete bytes, including signatures.
Recovery restores that object and verifies its signature when signing is required;
it does not create a replacement signature.
Use the executable that created an older active record to finish or reconcile it before upgrading.
Commit trailers can identify missing records; they cannot recreate publication authority.

## Validation and publication

[Release validation policy](config.md#release-policy) names the required GitHub workflows and jobs.
Evidence binds each workflow ID, run ID, attempt, and successful job ID to the release commit.
The executor queries the attempt-specific jobs endpoint.
It rejects self-authorization and rechecks retained evidence on resume.
A rerun cannot replace a verified attempt.

Cargo builds and uploads registry packages.
Cargo-Rail retains their digests
and uses Cargo's credential-provider protocol to compare the upload checksum
before returning publication credentials.
This is checksum-checked Cargo repackaging; it is not a prebuilt archive upload API.
An observed registry version must match the retained checksum and must not be yanked.
Local tag creation and required signature verification precede the first registry upload.
Pushing a tag compares its complete object identity, not only the commit it points to.

Each attempted upload has its own identity, persisted before Cargo receives its credential.
The local marker and remote record prevent another executor from claiming the same package upload
with a different attempt identity.
An attempted upload with an absent index entry remains uncertain.
Recovery does not repeat that upload automatically.

## Native assets

Optional `release.artifacts` policy declares one complete asset inventory per selected package.
The planner resolves `{crate}` and `{version}` filename templates before sealing intent.
Each producer must also appear in `release.validation`;
its required jobs own product-specific build and target validation.
Cargo-Rail does not infer a binary's architecture from its filename.

The producer uploads one flat ZIP artifact named `release-<crate>-<run-id>-<attempt>` through GitHub Actions artifact storage.
It contains exactly the declared release files.
The executor binds the artifact ID, original run and attempt, source, creation interval, expiry,
size, and SHA-256 digest.
It checks the downloaded ZIP and every extracted file before creating tags
or uploading a registry package.
A declared `source` file, such as a license, must match the regular Git blob at the prepared release commit.
Paths are relative to the Git root.

Recovery downloads the original artifact ID and verifies the sealed bytes again.
An expired, missing, or replaced artifact blocks execution;
a rerun does not authorize rebuilding sealed bytes.
ZIP inputs allow only flat regular files with portable names and stored or deflated compression.
Each package allows at most 64 files, 256 MiB per expanded file,
and 1 GiB for the archive and its expanded inventory.

GitHub draft recovery checks the exact source, tag, title, body, prerelease flag,
and asset inventory.
The record retains the observed release ID before asset uploads.
Recovery reads that release by ID. If creation succeeded before its ID was retained,
recovery searches the paginated release list and rejects multiple releases with the intended tag.
A partial draft can receive its missing assets;
existing assets must already match their sealed sizes and SHA-256 digests.
Publication requires a complete verified draft and a matching observable public release.
Conflicting published releases block recovery.

## Record retention

Local execution uses a filesystem lock.
Optional remote retention stores records in per-transaction Git notes refs, with one active pointer.
Each record commit contains `record.json` and, once Cargo packages are sealed,
a `packages` tree containing exactly those original `.crate` blobs.
Their sizes and SHA-256 digests must match the package seal.
Atomic updates use explicit expected ref values;
record identity and recoverable package bytes move together before the release branch is pushed.
Git retains these archives with the transaction history.
Remote object-size and storage limits apply; failed retention blocks publication.
Recovery rejects stale progress and preserves the original effect identities
after a lost acknowledgment.
The preparation push names the exact release commit
and requires the captured remote branch tip at the server.
A moved branch cannot authorize a new push, even when it would be a fast-forward.

Fetching a remote record also restores its original package archives on a fresh runner.
Fetch and import retain locally without publishing a record or moving the remote active pointer.
Archives must match the complete retained inventory and original digests.
Conflicting local archive bytes are preserved and reported instead of overwritten.

Hosted requests bind their executor workflow and retain exact validation dispatches.
Reviewed requests retain the PR number, original preparation,
and exact merged commit and tree under the same intent.
Alias intent captures the previous remote object;
promotion follows verified immutable publication and uses a Git lease.
See [releases](releases.md) for invocation and bootstrap.
