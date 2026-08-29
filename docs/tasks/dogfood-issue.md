# Cargo-Rail dogfood issues

**Status:** Implementation complete; hosted and cross-trust authenticated proof remains
**Reviewed:** 2026-08-29
**Cargo-Rail baseline:** `v0.24.0@7c59b78ccc684a76a623d446f43f686acae26450`
**Action baseline:** `v8@6e7355bfca7a308da74b1cc3487a539567adc286`
**Code authority:** `tools/compiler-fact-driver/`, `src/surface.rs`,
`src/commands/surface.rs`, `src/remote_cache/`, and the companion
`cargo-rail-action`

## Decision

Do not describe this repository as sharing one verified Cloudflare R2 cache
between CI and the remote development machines until CR-DOGFOOD-002 through
CR-DOGFOOD-005 are complete and production evidence shows remote reads and
writes from both trust domains.

The current Surface result is safe. Its retention summary is not an actionable
explanation of that safety. The current cache setup is locally healthy, but
local health does not prove that any remote provider request succeeded.

## What the current Surface counts mean

The current two-view Surface run captured 20 compiler fragments containing
59,532 item observations and merged them into 46,652 graph nodes. It reported
17,164 conservative retention observations affecting 15,597 unique merged
items:

- 3,373 `allow-dead-code` observations affecting 3,097 unique items;
- 1,320 `generated-registration` observations affecting 1,320 unique items;
  and
- 12,471 `unresolved-trait-dispatch` observations affecting 12,370 unique
  items.

These are not findings, source defects, or necessarily unique declarations.
`SurfaceCompleteness` sums each fragment's retention rows before the graph
merges repeated observations across production, test, and doctest units.

The compiler fact driver currently emits `unresolved-trait-dispatch` for every
definition whose immediate parent is a trait or a trait implementation. It is
not reporting 12,191 failed call-site resolutions. It is recording that the
driver did not prove a narrower dispatch closure for those declaration
observations.

`generated-registration` is limited to non-production macro-generated
constants that register a written function. Retaining those roots prevents
test and similar harness registrations from being classified as dead.

`allow-dead-code` records that rustc's effective `dead_code` lint level at the
definition is `allow`. That level may be inherited or generated; the current
report does not identify its provenance.

## CR-DOGFOOD-001: Make Surface retention evidence understandable

**Evidence:** Explainability resolved; measured precision change not justified

`cargo rail surface --check --explain` renders only one aggregate count per
retention reason. The output does not say that the counts are fragment
observations, show how many unique merged declarations they affect, separate
production from non-production roots, identify inherited lint policy, or show
which targets and views dominate a reason. The name
`unresolved-trait-dispatch` is especially misleading because the implemented
predicate is trait or trait-implementation membership, not one unresolved
dispatch call site.

Surface report contract v3 resolves the explainability slice. It preserves the
raw completeness totals, adds the merged denominators above, reports raw and
unique counts per predicate, and includes at most three deterministic physical
declaration examples with the exact implemented predicate. Text output calls
these conservative proof observations rather than issues. A repeated
production/non-production fixture proves that raw rows merge without losing a
conservative root or double-counting the unique item.

`--explain` now measures one precision question separately: omit one retention
reason while keeping every other reason and graph authority active, then count
newly eligible findings before diagnostic policy. The 2026-08-29 repository
measurement found zero suppressed findings for `allow-dead-code`,
`generated-registration`, and `unresolved-trait-dispatch`. The normal path
remained three traversals and 177,914 edge visits. Explanation added six
traversals and 408,109 edge visits. Compiler acquisition dominated the command,
so this single run supports no wall-time claim.

There is therefore no evidence for narrowing trait dispatch, and its
fail-closed semantics remain unchanged. Domain, target-view, generated-source,
and lint-provenance breakdowns are a separate possible observability expansion;
they should grow the contract only when a consumer demonstrates an actionable
need.

## CR-DOGFOOD-002: Expose root portability in the cache Action

**Evidence:** Implementation complete; Action publication remains

The companion Action now accepts typed `physical` and `remap` values, defaults
compatibly to `physical`, passes the value through its one setup transaction,
and includes the selected policy in its redacted status projection. Contract
tests cover both values and reject invalid input. This repository pins that new
Action commit, selects `remap`, and no longer repeats setup through a local
script.

The pin is exact but the Action commit and its release ref remain unpublished
until this work is reviewed and pushed.

## CR-DOGFOOD-003: Add a remote cache readiness proof

**Evidence:** Product and Action contracts complete; authenticated hosted proof remains

`cargo rail cache probe` reuses the persisted authority, authenticated object
transport, and protocol-marker implementation. It reports only provider, mode,
readiness, and marker state. Read mode requires a compatible marker; read-write
mode may initialize an absent marker safely. Authentication, authorization,
transport, and incompatible-protocol failures remain distinct diagnostics.

The companion Action exposes a strict probe input in its single setup
transaction and preflights command support before making setup changes. Its
default remains non-strict for compatibility. Cargo-Rail compilation retains
its correctness-first remote-failure fallback. The repository's source-built
R2 qualification probes both producer and read-only consumer jobs explicitly;
the hosted transaction cannot run until the commits are pushed.

## CR-DOGFOOD-004: Complete the Cloudflare R2 contract and proof

**Evidence:** Contract and workflow complete; hosted transaction remains

On 2026-08-29 the repository selected one normalized R2 authority for trusted
`main` and release compiler work. Wrangler confirmed that the URL account,
private bucket, standard endpoint placement, multipart-abort rule, and
`native-v5/entries/` lifecycle prefix agree. CI now rejects a selected remote
when either repository secret is absent, exposes credentials only to steps
that execute compiler work, gives reusable workflows named secrets, and gives
pull requests no remote URL or credential. Legacy AWS OIDC wiring is no longer
part of the repository contract.

The source-built qualification workflow now uses a disposable prefix, probes
authenticated readiness, publishes verified native-cache results, restores
them from a separate read-only job rooted at a distinct fixture path, validates
the two evidence packs, and removes objects plus incomplete multipart uploads
in an unconditional cleanup job. Hosted producer/consumer evidence remains
open until the reviewed commits are pushed.

Cargo-Rail's `r2://` parser requires a 32-hex account ID, rejects query
parameters, and hard-codes the default
`https://ACCOUNT_ID.r2.cloudflarestorage.com` endpoint with region `auto`.
Cloudflare jurisdictional buckets require a jurisdiction-specific endpoint,
so they cannot currently be selected. The repository must use a default-
jurisdiction bucket until the URL contract grows a typed jurisdiction without
accepting arbitrary endpoints.

Parser tests now cover the accepted 32-hex account, derived provider endpoint,
and rejection of noncanonical accounts, ports, queries, and jurisdiction
syntax. The companion tests cover accepted R2 projection, credential and
session-token transport, and fail-closed invalid input. Provider documentation
records private default-jurisdiction buckets, scoped API credentials, standard
AWS environment variables, marker-preserving lifecycle rules, and metered
storage and operations.

## CR-DOGFOOD-005: Make remote development machines join the same authority

**Evidence:** Operator adapter complete; live cross-root proof remains

The external `dev-machines` adapter now:

1. selects the Cargo-Rail cache path on every provider;
2. receives the one canonical `r2://ACCOUNT_ID/BUCKET/PREFIX` authority;
3. receives `read-write` only for trusted ephemeral machines;
4. receives R2 credentials through private machine state, including
   `AWS_SESSION_TOKEN` when short-lived credentials are used;
5. runs `just rail-cache-setup --max-size 10GiB` during bootstrap;
6. persists `--root-portability remap` through that repository front door;
7. removes or expires credentials with the machine lease; and
8. records distinct producer and consumer fixture-root identities in the
   retained evidence pair.

Use separate bucket-scoped credentials for GitHub and remote machines even
though they address one cache. A single cache means one bucket and namespace,
not one long-lived secret copied into every trust domain. Live bidirectional
cross-root traffic is deliberately deferred to final remote qualification.

Final remote qualification was attempted after the local and native-musl gates passed. It stopped before cache
traffic because the configured macOS Keychain item contained no R2 parent credential. The failed bootstrap created no
cache authority, and its instances and EBS volumes were deleted. The operator adapter is complete, but bidirectional
cross-root reuse remains unproved until an authorized parent credential is installed. No Windows host was provisioned
merely to reproduce the same missing-authority failure.

## Completion evidence

This task is complete only when:

- Surface explains retention observations and their unique impact without
  implying thousands of code defects;
- one cache setup transaction selects `remap` in the released Action;
- CI proves authenticated R2 readiness rather than only local installation
  health;
- a trusted main job publishes an entry and a separate trusted job restores
  it;
- at least one AWS, Azure, or Latitude machine restores an entry produced by
  GitHub Actions, and GitHub Actions restores an eligible entry produced by a
  remote machine;
- the evidence identifies one normalized remote authority and contains no
  credential values; and
- provider failure still executes the normal compiler path with an observable
  stable reason.
