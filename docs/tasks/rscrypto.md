# Cargo-Rail consumer issues

## Release status and undeclared independent lockfiles

Status: released and authenticated on 2026-10-11 in [Cargo-Rail 0.31.1](https://github.com/loadingalias/cargo-rail/releases/tag/v0.31.1)
and [Cargo-Rail-Action 11.0.1](https://github.com/loadingalias/cargo-rail-action/releases/tag/v11.0.1).
Permanent rscrypto adoption and shared-cache qualification remain separate gates.
Cargo-Rail 0.31.0 reproduces both defects despite the earlier consumer task's administrative
closure.

- Release status derives required effects from the retained intent.
  Text and JSON identify skipped effects without counting them as completed publication
  or next work.
  Interrupted preparation keeps its ambiguity and recovery command.
  The stored release-plan v10 and release-record v11 contracts remain unchanged.
- Release planning inspects captured tracked independent lockfiles.
  Cargo metadata confirms the selected package's actual manifest path;
  an unrelated package with the same name and version does not block the release.
  Omitted workspaces must be declared in `release.auxiliary_cargo_manifests` before preparation.
  Capture follows inactive optional and transitive path dependencies.
  Unattributed local lockfile identities also require an explicit declaration.
  Discovery authorizes no additional writes.

The two defect regressions fail on the original source.
The source-built retained probe now rejects the omission in ordinary check, extended check,
and execution; the unchanged fixture still passes locked metadata.
The declared control updates its lockfile and passes locked metadata after preparation.
Preparation-only status, registry evidence, forge publication,
and existing recovery regressions pass locally.
The unpublished-dependent, per-crate tag, warning,
and declared-Cargo-write regressions remain covered.
Complete local validation passed: `just check` ran 1,789 tests, six doctests,
the dedicated compiler-driver checks, and all six cross-target lint lanes.
All 17 focused release regressions pass.
The final native archive passed checksum/component verification and all 15 installation checks.
The unchanged Action source passed its complete check
and all seven ignored CLI contract tests against the final candidate,
including independent release-record validation.
The new source-bound evidence is retained in rscrypto's `benchmark_results/cargo-rail-release-repair-20261011T175442Z/REPORT.md` and its verified sibling archive.

The fixes are pushed in `3b5a72800973d1356c7eba7b37b5a98af545595f`.
A test-only Windows diagnostic-path correction is pushed in `371affd0a45c1a62c26a739f69212989a70aa856`;
[source CI](https://github.com/loadingalias/cargo-rail/actions/runs/38169338124) passed all 13 jobs.
The owner then authorized both releases through Cargo-Rail's transaction engine.
Cargo-Rail 0.31.1 released commit `ec6d396738b8ad9204236c088c3e3ffea9440adf`: its [executor](https://github.com/loadingalias/cargo-rail/actions/runs/38177591767),
[prepared-source CI](https://github.com/loadingalias/cargo-rail/actions/runs/38177971615), and [packaging](https://github.com/loadingalias/cargo-rail/actions/runs/38177977323) succeeded.
The crates.io package matches that commit.
All declared assets passed immutable-release verification;
the four native archives and host manifest passed source-bound provenance verification.
The authenticated native release passed all 15 installation checks, both defect probes,
and six unpublished-dependent/tag-format regression cases.

Cargo-Rail-Action 11.0.1 released commit `2104429578cfeef1a6a5beb379c2202e0310df78`.
Its CLI/tooling lock and all four public defaults select Cargo-Rail 0.31.1.
The stored plan v9, release-plan v10, and release-record v11 contracts remain unchanged.
Its [source CI](https://github.com/loadingalias/cargo-rail-action/actions/runs/38182970178), [executor](https://github.com/loadingalias/cargo-rail-action/actions/runs/38185569613), and [prepared-source packaging](https://github.com/loadingalias/cargo-rail-action/actions/runs/38185653741) succeeded.
All six declared assets passed immutable-release and source-bound provenance verification.
The published native Action runtime independently validates both terminal release records.
The `v11` alias and immutable `v11.0.1` tag resolve to the same released commit.

On clean rscrypto at `cbf72d905c8ff5f88ed43fec4b65251161ee541a`, a per-process PATH override selected the authenticated published CLI.
`just release-check` exited 0; its direct JSON check exited 1 for pending preparation.
Both planned only rscrypto and selected `v0.10.0` → `v0.11.0` without warnings.
Tracked source, index state, tooling pins, and the global CLI were unchanged by this preview.
The new evidence and read-only CI release audit are retained in rscrypto's `benchmark_results/cargo-rail-coordinated-release-20261011T214119Z/REPORT.md`
and its verified sibling archive.
The earlier sealed bundles remain unchanged.

Remaining gate: rscrypto Track 1 permanently adopts the authenticated CLI,
updates its existing version and platform asset URLs/checksums, and pins the Action to `2104429578cfeef1a6a5beb379c2202e0310df78`.
It then repeats the defect probes, unpublished-dependent/tag-format regression,
and clean `just release-check` after adoption.
The check must exit 0 or 1, plan only rscrypto, and select `v0.10.0` → `v0.11.0`.
No alias tags or change to rscrypto's tag-creation logic are required.
Track 2 and provider source ownership remain unchanged.
No current Action defect is reproduced.
Shared-cache producer/reader reuse remains unproved and is a separate qualification gate.

## Release one crate while an unpublished member depends on it

Status: released in Cargo-Rail 0.31.0; rscrypto adoption verified at `614d1b56`.
The owner prioritized this fix on 2026-10-10.

The root workspace will contain `rscrypto` and `rscrypto-rustls`.
The provider has `publish = false` and depends on `rscrypto` by path and version.
Release `rscrypto` v0.11 before the provider's first release.
Keep rscrypto's `v<version>` tag line and give the provider `rscrypto-rustls-v<version>` tags.

The retained generic probe is `/tmp/rail-tag-probe-lrXz`.
Cargo-Rail 0.30.1 rejects patch, minor, and path-only subset releases.
`--include-dependents` requires a provider change file.
A first provider release incorrectly uses the other crate's previous tag,
and local release-check JSON omits the collision warning.

Implemented policy:

- Separate selected package releases from necessary Cargo manifest and lockfile writes.
- Exclude `publish = false` members from implicit dependent closure.
  Keep the strict rule for published dependents, including transitive dependents.
- Preserve compatible requirements of unselected packages and path-only declarations.
  Retarget an incompatible versioned path requirement without changing the dependent's package
  version.
  Reject ambiguous edits before mutation.
  Selected released dependents retain the existing minimum-version refresh,
  including their shared workspace requirements.
- Project primary and declared auxiliary Cargo workspaces from captured source;
  validate the resulting lockfiles and retain exact writes for apply and recovery.
- Resolve optional `[crates.NAME.release].tag_format` overrides for both previous and new tags.
  Keep the global format as the default.
- Carry the same warnings in text and JSON output.

Proof: generic regressions demonstrated the original failures and now cover patch, minor, path-only,
`--include-dependents`, published dependents, per-crate previous tags, and warning parity;
apply fixtures must pass `cargo metadata --locked` and leave the unpublished member's version, changelog, change files,
and tags unchanged.
Configuration and release policy docs describe these decisions.
The companion Action's independent reader qualifies the stored-contract transition.

Adoption configuration for Cargo-Rail 0.31.0:

```toml
[release]
tag_format = "{prefix}{version}"

[crates.rscrypto-rustls.release]
tag_format = "{crate}-{prefix}{version}"
```

No alias tags or change to rscrypto's current release workflow are required.
The retained consumer evidence verifies that the pinned release plans only rscrypto,
with previous tag `v0.10.0` and tag `v0.11.0`.
Repeat this acceptance after adopting the release-status and lockfile-discovery patch.

## Cargo-Rail omits the Windows ARM64 native archive (0.29.0 and 0.30.0)

Status: released in Cargo-Rail 0.31.0 using the owner-selected archive-host manifest.
The consumer pins are adopted; native platform qualification remains separate.
Historical evidence from 2026-09-30: the v0.30.0 release still publishes no `cargo-rail-aarch64-pc-windows-msvc.zip`,
while its `.config/rail.toml` still lists `aarch64-pc-windows-msvc`.

Command: `just update` in rscrypto after changing the Cargo-Rail archive suffix to match the 0.29.0 release.

Context: rscrypto supports `aarch64-pc-windows-msvc` and its local Windows installer consumes the exact Cargo-Rail version
and native archive recorded in `.config/tooling.toml`.

Expected: the latest stable Cargo-Rail release publishes one authenticated native archive
for every host in its consumer-facing release matrix, including `cargo-rail-aarch64-pc-windows-msvc.zip`,
or publishes an explicit machine contract that lets consumers select source installation
for unsupported archive hosts.

Actual: Cargo-Rail 0.29.0 publishes ZIP archives for AArch64 macOS, AArch64 Linux, x86-64 Linux,
and x86-64 Windows.
Its release configuration does not declare an AArch64 Windows artifact.
The rscrypto updater fails with `loadingalias/cargo-rail: required native release asset is missing: cargo-rail-aarch64-pc-windows-msvc.zip`.

Evidence: the immutable 0.29.0 GitHub release contains no AArch64 Windows archive.
The tagged `.config/rail.toml` declares only the four published archive paths under `release.artifacts.cargo-rail.files`,
while the same configuration still lists `aarch64-pc-windows-msvc` as a Tier 2 target-resolution view.
Cargo-Rail 0.25.0 did publish `cargo-rail-aarch64-pc-windows-msvc.zip`.

Current mitigation: rscrypto installs Cargo-Rail 0.29.0 from source on Windows ARM64
when the native archive is absent.
Other native hosts continue to use authenticated release archives.
`just update` therefore completes without retaining an older Cargo-Rail binary.

Implemented: `cargo-rail-archive-hosts-v1.json` derives its exact host-to-archive map from `release.artifacts.cargo-rail.files`.
Packaging rejects missing or extra archives, adds the manifest to `SHA256SUMS`,
and includes it in build-provenance attestation.
Its schema requires the release version and `unsupported_host_installation = "source"`.
Windows ARM64 remains a supported target-resolution view with source installation.
The generator and schema regressions cover the declared inventory and failure cases.
Remove rscrypto's source-install fallback only after a released archive
and native installation evidence exist.

## Cargo-Rail Unify misses unused dependencies outside the root workspace and misplaced test-only ones

Status: scope and misplacement released in Cargo-Rail 0.31.0; the consumer pins are adopted.
Recorded 2026-09-26.
The rscrypto findings are fixed in `ecc7d47e` (Cargo's lint now reports nothing).
The historical task file `cargo-rail/docs/tasks/unify-unused-dependencies.md` no longer exists.
This entry retains the evidence and records the implemented scope and misplacement repairs below.

Command: `cargo rail unify --explain` (preview only) in rscrypto and in a throwaway probe workspace (`/tmp/rs-land/unused-probe/make.sh`).

Context: Cargo on `nightly-2026-09-25` adds the warn-by-default `cargo::unused_dependencies` lint.
The independent-workspace lint reports three fuzz packages:
`auth-phc` and `hash-websocket-sha1` declare `rscrypto_fuzz` as a normal dependency used only by `tests/corpus_replay.rs`, and `hash-blake2` declares `hmac` and uses it nowhere.
Unify never reported them.

Expected: Unify, which compiles to prove unused dependencies,
reports every unused or misplaced declaration in the Cargo manifests the repository declares to
Cargo-Rail.

Actual:

1. Scope: `UnusedDepFinder` analyzes only root-workspace members. rscrypto's 35 fuzz packages are independent
   workspaces listed only in `release.auxiliary_cargo_manifests`, which drives release lockfile updates, not Unify.
   Their manifests are never analyzed.
1. Misplaced test-only dependency: in the probe,
   a normal dependency used only by an integration test is kept silently.
   Cargo reports it and suggests `dev-dependencies`.
   Unify neither removes it (correct: removal would break the test) nor reports the misplacement.
1. Shared blind spot, not specific to Unify:
   a direct dependency used only through another crate's re-export (`outer::inner`) is treated
   as used by both Unify and Cargo, because both rely on rustc's `unused_crate_dependencies`.
   Example: `hash-blake2` declares `digest` but uses it only as `blake2::digest`.

Evidence: probe workspace with `dead` (unused), `testonly` (test-only), and `inner` (re-export only).
Cargo flags `dead` and `testonly`; Unify flags only `dead` (`1/1 applicable configurations complete; 1 unused`).
Source: `src/cargo/unify/unused_finder.rs` ("Detects unused dependencies in workspace members"), `src/compiler/collector.rs`
(`unused_crate_dependencies` evidence; normal domain excludes test-mode units).

Impact: Unify can pass while auxiliary manifests carry dead or misplaced dependencies.
In rscrypto, `just check` now surfaces Cargo's lint for every independent workspace,
which covers the gap until Unify does.

Implemented: `unify.cargo_manifests` declares independent workspace roots explicitly,
separately from release lockfile policy.
Each workspace has its own Cargo capture, resolution graph, snapshot, coverage views,
and dependency repair proofs.
Apply includes its exact files in drift checks, backup, rollback,
and post-edit resolution verification.

Normal-domain unused evidence plus development-domain retention
and captured identifiers confined to integration tests, benches,
or examples can authorize a move to the matching development table.
Production source and README references retain the existing conservative behavior.
The original key, dependency value, rename, features, target condition, and comments are preserved.
An existing destination entry remains a manual review case.
A regression uses identical package names in separate workspaces, removes a dead path dependency,
moves a renamed target-specific test dependency, and runs both workspaces' locked tests after apply.

The re-export case is an explicit remaining compiler limitation, as the optional U3 task allows.
No source spelling heuristic removes dependencies.
Documentation distinguishes Cargo's single-build lint from Unify's configured-view proof and editor.

## Release planning silently skips the semver gate when no tag matches `tag_format`

Status: released in Cargo-Rail 0.31.0; the consumer pins are adopted.
Found 2026-10-01 with cargo-rail 0.30.1. rscrypto works around it by declaring its tag format.

Command: `just release-check` (`cargo rail release check rscrypto --bump auto --skip-tag`) on `main` at `b41e5e5c`, with `release.semver_check = "deny"` and no `release.tag_format`.

Context: rscrypto's release workflow tags `v<version>`.
Tags `v0.7.1` through `v0.9.0` exist and `v0.9.0` is an ancestor of `HEAD`; crates.io has 0.9.0.
The effective default `release.tag_format` is `{crate}-{prefix}{version}`, so cargo-rail looks for `rscrypto-v0.9.0`.

Expected: with `semver_check = "deny"`, a missing baseline for a crate that has published versions fails the plan
(or reports which tag names it tried), because "deny" promises conclusive compatibility evidence.
A single-crate workspace could also default to `{prefix}{version}`.

Actual: the plan reports `Bump: ... (auto: reviewed change files -> minor (no previous tag: full history))` and `API: NotApplicable: first release has no prior release tag to compare`, and exits as a normal "changes detected" preview.
The semver check against 0.9.0 never runs,
and the history scope is every commit instead of the commits since 0.9.0.

Impact: a release can pass preparation with no API compatibility evidence while the policy says `deny`.
Earlier releases may have had the same silent gap.

Mitigation: `.config/rail.toml` sets `release.tag_format = "{prefix}{version}"`.

Implemented: `deny` requires a matching Git baseline or positive crates.io evidence
that no version has been published.
A published or yanked version, unavailable registry response, malformed response,
or missing comparison remains unavailable and blocks release.
`warn` reports the missing baseline without treating it as a proven first release; `off` remains disabled.
Deterministic registry and API-policy regressions cover these decisions.
Tag formats retain their existing default; per-crate overrides select independent baselines.

## Surface target preflight cannot pass for `no_std` targets

Status: released in Cargo-Rail 0.31.0 and qualified with generic fixtures;
rscrypto's selected target matrix remains to be checked after adoption.
Found 2026-10-01 with cargo-rail 0.30.1 while preparing rscrypto v0.10.0.
It blocks rscrypto's `just release-prepare`, whose first step is `cargo rail surface --check --explain`.

Context: rscrypto sets `[surface] targets = "workspace"` (host plus 15 catalog targets, including `aarch64-unknown-none`, `riscv32imac-unknown-none-elf`, `thumbv6m-none-eabi`, and `x86_64-unknown-none`).
Host: macOS AArch64, `nightly-2026-09-30`, every target's `rust-std` installed.

Expected: Surface analyzes declaration reachability for every selected view.
The preflight probe for a target without `std` is a `#![no_std]` crate
(or a check-only probe),
and a target whose linker is absent gets a check-only path or an explicit, reported skip.

Actual: "Surface target preflight failed before compiler acquisition" after 14 s
for every non-host target.
Bare-metal targets: `error[E0463]: can't find crate for std`, because the probe did not declare `#![no_std]`, and `#[panic_handler] function required`.

Linux GNU/musl targets: `linking with cc failed`.
Windows MSVC targets: `linker link.exe not found`.
The hint, "configure its real SDK/linker environment", cannot fix the bare-metal rows,
and no single host has both Linux cross-linkers and MSVC.

Impact: with `surface.targets = "workspace"`, no machine can pass the gate, so a consumer's release procedure cannot run.
Host-only Surface (the same config without `surface.targets`) does run,
and it found two real feature-gating build failures in rscrypto
(fixed in its `53842684` and `72c872eb`), so the analysis itself is valuable.

Implemented scope:

1. Cross-target preflight uses a check-only `no_std` library probe.
   Host doctests retain their host link requirement.
1. Cross-target typed acquisition uses production Cargo targets without linking.
   Tests, examples, benchmarks, and doctests run only in a selected host view.
1. Surface report v5 records those view scopes and incomplete target/doctest coverage explicitly.
   Visibility repair rejects cross-target scope because omitted test views cannot authorize edits.

Regression: a `#![no_std]` library is analyzed with a deliberately unusable Linux linker
and the installed `thumbv6m-none-eabi` target.
JSON validates against report v5 and records host-only test/doctest scope;
visibility repair rejects before source mutation.
This proves the generic engine path locally. rscrypto must run its complete selected matrix
after adopting the released binary; this session does not edit or qualify rscrypto.

### Completion and release handoff

Cargo-Rail 0.31.0 releases the required fixes above and closes installation-recovery tasks I1
and I2.
Stale authenticated components now report exact recovery evidence;
`cargo rail cache setup` repairs a deleted managed installation, including without a receipt.
Archive and source-installation regressions qualify those behaviors,
and a deleted-directory regression restores setup and builds successfully afterward.
The optional re-export limitation and broader B3 automatic installer remain separate work.

rscrypto acceptance remains a separate Track 1 gate.

Qualification on 2026-10-10 and 2026-10-11:

- `just check` passes formatting, Markdown, host Clippy, dependency policy, documentation,
  compiler-driver checks, all 1,784 tests, six executable documentation examples,
  and Clippy for all six configured cross targets.
- `just check-tooling` passes, including archive-host generation and schema checks.
- All nine focused release regressions pass after final review,
  including the public planner's caller-supplied global tag format and per-crate override.
- The companion Action's complete check passes 77 tests.
  All 11 ignored contract tests pass with the authenticated Cargo-Rail 0.31.0 binary and archive,
  including installation through the updated lock without a Rust toolchain.

  The final portable Cargo-write ordering regression also passes in both producer and reader.
- Generic fixtures reproduce the 0.30.1 subset-release and tag failures,
  then pass with the source fix.

  Release preparation leaves every projected workspace valid under `cargo metadata --locked`.

  Cross-target Surface qualification checks a real `no_std` library with no usable cross linker.

Release-plan v10 and release-record v11 store exact Cargo writes.
The companion Action's independent v11 schema, reader,
and rejection tests are updated and qualified against the published producer.
Its lock and all four Action inputs now select Cargo-Rail 0.31.0.
Cargo-Rail-Action 11.0.0 publishes the matching consumer contract.
Finish older active release transactions with their originating executable.

Published on 2026-10-11:

- [Cargo-Rail v0.31.0](https://github.com/loadingalias/cargo-rail/releases/tag/v0.31.0) tags `41658c89b61e61679c9a23c864ae450df2a7b85f`.
  Durable transaction `release-263cac39e673` is complete.
- [Final source CI](https://github.com/loadingalias/cargo-rail/actions/runs/38103049377) and [prepared-release CI](https://github.com/loadingalias/cargo-rail/actions/runs/38105428763) pass all 13 jobs, including native POWER, RISC-V,
  and IBM Z suites.
  Prepared-release CI completed at attempt 3 after recovering a Cargo download checksum failure
  and an Ubuntu snapshot HTTP 503 failure.
  GitHub retained the successful jobs in the final attempt;
  every required job remains bound to the exact release commit.
- [Packaging](https://github.com/loadingalias/cargo-rail/actions/runs/38105434190), attempt 1, passes all four native archives and installation checks.
  Build provenance binds the archives and archive-host manifest to the release commit.
  The immutable GitHub release and all seven assets verify against the qualified files.
  The published crates.io package and index checksum match the sealed package bytes.
- [Cargo-Rail-Action v11.0.0](https://github.com/loadingalias/cargo-rail-action/releases/tag/v11.0.0) tags `98167d6d7fbea02d1994fb292f049defd49cf6a6`.
  Durable transaction `release-e120dda9df2b` is complete.
  [Source CI](https://github.com/loadingalias/cargo-rail-action/actions/runs/38112241797) passes all four native hosts.
  [Prepared-release packaging](https://github.com/loadingalias/cargo-rail-action/actions/runs/38114499074) passes all four native checks and asset collection at attempt 1.
  Build provenance binds all six assets to the exact release commit;
  the immutable release signature and every downloaded asset verify against the sealed package.
  The published macOS runtime passes its version and target self-check.
  Alias `v11` selects this release; `v10` remains unchanged.

Native qualification also corrected the Ubuntu kernel-tool snapshot, Windows reserved-name fixtures,
rustc's nightly entry-point rename, and the Surface fixture's foreign-target selection.
Stable, beta, and nightly compiler-driver checks pass.
The RISC-V diagnostic fixture remains in place; its final native suite passes.

rscrypto Track 1 adopted Cargo-Rail 0.31.0 and immutable Cargo-Rail-Action 11.0.0 pins.
The retained acceptance at `614d1b56` plans only rscrypto and selects `v0.10.0` -> `v0.11.0`.
The separate release-status and undeclared-lockfile defects above require another Cargo-Rail
release.
The existing tag configuration needs no alias tags or release-workflow change.
