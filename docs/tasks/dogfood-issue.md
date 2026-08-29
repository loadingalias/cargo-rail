# Cargo-Rail dogfood issues

**Status:** Open; product, Action, and operator-integration work required
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
58,362 item observations and merged them into 45,692 graph nodes. It reported
16,844 conservative retention observations:

- 3,369 `allow-dead-code` observations;
- 1,284 `generated-registration` observations; and
- 12,191 `unresolved-trait-dispatch` observations.

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

**Evidence:** Confirmed product explainability gap; analysis-precision
investigation required

`cargo rail surface --check --explain` renders only one aggregate count per
retention reason. The output does not say that the counts are fragment
observations, show how many unique merged declarations they affect, separate
production from non-production roots, identify inherited lint policy, or show
which targets and views dominate a reason. The name
`unresolved-trait-dispatch` is especially misleading because the implemented
predicate is trait or trait-implementation membership, not one unresolved
dispatch call site.

Required work:

1. Preserve raw observation counts for acquisition completeness, but add
   unique merged-item counts per retention reason.
2. Break retention evidence down by domain, target, compiler view, written
   versus generated provenance, and lint-level provenance where rustc exposes
   it reliably.
3. Make text output call these values conservative proof observations, not
   issues. Include the total fragment observations and unique graph nodes so
   the denominator is visible.
4. Add a bounded explanation mode that identifies representative retained
   declarations and the exact predicate that retained each one without
   flooding terminal output.
5. Measure how many otherwise eligible findings each reason suppresses. Do
   not call a large count an analysis defect until this counterfactual is
   known.
6. Investigate narrower trait-dispatch authority from compiler facts. Any
   reduction must remain fail-closed for generic, dynamic, associated-item,
   and cross-crate dispatch.

Acceptance requires counts that reconcile from fragments to unique graph
items, stable machine fields or an explicit schema transition, fixtures for
repeated production and non-production observations, and no loss of a
conservative root.

## CR-DOGFOOD-002: Expose root portability in the cache Action

**Evidence:** Confirmed companion-action contract gap

Action v8 runs `cargo rail cache setup` without `--root-portability`. Physical
root identity is therefore the default. This repository must immediately run
the same setup a second time through a local script with
`--root-portability remap` so GitHub checkout roots and remote development
checkout roots can share the certified portable result classes.

Add an optional, typed `root-portability` input to the cache Action with
`physical` as the compatibility default and `remap` as the only other value.
Pass it through the single setup transaction, include the selected mode in the
redacted status projection, and test both values plus invalid input. Release
the addition through the Action release train; do not mutate the pinned v8
commit in place.

The repository workaround can be removed only after the newly released Action
is pinned and its status output proves `remap`.

## CR-DOGFOOD-003: Add a remote cache readiness proof

**Evidence:** Confirmed product and companion-action observability gap

The cache Action reports a healthy local installation and
`direct_transport_selected`, then explicitly says that status inspection did
not contact the provider. Invalid credentials, a missing bucket, an
incompatible or absent protocol marker, and a network failure can therefore
produce a green setup step. Cargo-Rail intentionally falls back to compilation
on remote failure, so CI can silently stop sharing results while remaining
correct.

Add a bounded Cargo-Rail remote probe that uses the installed authority and
credentials, redacts the URL and object identities, and distinguishes at
least:

- authenticated read access and compatible protocol marker;
- an uninitialized read-write authority that can be initialized safely;
- authentication or authorization failure;
- transport failure; and
- incompatible protocol state.

The cache Action should expose the probe result separately from local health
and support a strict input that fails a cache-seeding job when remote readiness
is not proved. Ordinary compilation must keep its existing correctness-first
fallback.

## CR-DOGFOOD-004: Complete the Cloudflare R2 contract and proof

**Evidence:** Repository integration complete; Cargo-Rail and Action follow-up remains

On 2026-08-29 the repository selected one normalized R2 authority for trusted
`main` and release compiler work. Wrangler confirmed that the URL account,
private bucket, standard endpoint placement, multipart-abort rule, and
`native-v5/entries/` lifecycle prefix agree. CI now rejects a selected remote
when either repository secret is absent, exposes credentials only to steps
that execute compiler work, gives reusable workflows named secrets, and gives
pull requests no remote URL or credential. Legacy AWS OIDC wiring is no longer
part of the repository contract.

This proves configuration, not a provider transaction. The bucket was empty
at inspection time, and Action v8 still cannot prove authenticated remote
readiness. CR-DOGFOOD-003 and the producer/consumer evidence below remain open.

Cargo-Rail's `r2://` parser requires a 32-hex account ID, rejects query
parameters, and hard-codes the default
`https://ACCOUNT_ID.r2.cloudflarestorage.com` endpoint with region `auto`.
Cloudflare jurisdictional buckets require a jurisdiction-specific endpoint,
so they cannot currently be selected. The repository must use a default-
jurisdiction bucket until the URL contract grows a typed jurisdiction without
accepting arbitrary endpoints.

The companion Action's R2 test passes the syntactically invalid URL
`r2://account/bucket/prefix` to a fake Cargo command. Its fake status also does
not prove a real R2 normalization or transport boundary. Add contract tests
against Cargo-Rail's accepted 32-hex URL, provider projection, credential
environment, session-token support, and fail-closed invalid URL behavior.

Add provider-specific documentation covering default-jurisdiction bucket
creation, bucket-scoped R2 API credentials, the standard AWS credential
environment consumed by Cargo-Rail, non-public bucket policy, and lifecycle
rules that expire only `native-v5/entries/` while preserving the protocol
marker. Document that R2 has free included usage and zero egress charges, not
unlimited free storage or operations.

## CR-DOGFOOD-005: Make remote development machines join the same authority

**Evidence:** Confirmed operator-integration defect

The current `dev-machines` project routing recognizes `rail` as a Cargo-Rail
cache consumer, explicitly excludes `cargo-rail` from the generic remote-cache
path, and therefore configures neither path for this repository. Bootstrap
does not run `rail-cache-setup`, and the generated machine environment does not
carry R2 credentials. AWS, Azure, and Latitude instances cannot currently
share the repository's CI authority.

Fix the external adapter so the `cargo-rail` project:

1. selects the Cargo-Rail cache path on every provider;
2. receives the one canonical `r2://ACCOUNT_ID/BUCKET/PREFIX` authority;
3. receives `read-write` only for trusted ephemeral machines;
4. receives R2 credentials through private machine state, including
   `AWS_SESSION_TOKEN` when short-lived credentials are used;
5. runs `just rail-cache-setup --max-size 10GiB` during bootstrap;
6. persists `--root-portability remap` through that repository front door;
7. removes or expires credentials with the machine lease; and
8. proves a cross-root producer/consumer pair against the same exact object
   authority before claiming sharing.

Use separate bucket-scoped credentials for GitHub and remote machines even
though they address one cache. A single cache means one bucket and namespace,
not one long-lived secret copied into every trust domain.

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
