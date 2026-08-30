# Changelog




## [0.25.0](https://github.com/loadingalias/cargo-rail/compare/v0.24.0...v0.25.0) - 2026-08-30

- Allow repository checks invoked by Cargo-Rail's release push to recognize change files already consumed by that release transaction.

- Add an authenticated remote-cache probe that validates the selected object store and exact protocol marker without exposing its URL or credentials.

- Use the companion cache Action's single portable setup transaction, tighten Cloudflare R2 URL coverage, and document
  the bounded default-jurisdiction and authenticated probe contracts.

- Dogfood Cargo-Rail v0.24 planning, dependency policy, compiler caching, Surface, and exact-SHA release archive reuse throughout local and CI workflows.

- Expose raw and merged Surface retention evidence with bounded examples, and measure omit-one-reason suppression only
  when `surface --explain` requests the additional graph work.

- Use rustc's complete definition-path identity for compiler facts and reject incompatible protocol-v3 facts after the protocol-v4 transition.

- Widen variant-scoped work to every row whenever any required path, configuration, or Cargo input is not attributed by the selected catalog rows.

- Model deliverables with typed Cargo roots and external paths, emit runtime artifacts as distinct named Cargo work,
  and expose strict Cargo-scope and package-name projections to plan consumers.

- Ship authenticated Surface authority in both native Linux musl archives, with exact-host driver manufacture, dynamic
  musl loader proof, warm fact reuse, and stable and dated-nightly source fallback qualification.

- Plan and commit exact post-release lockfiles for declared standalone Cargo manifests, with release plan/state v5 recovery binding and no generic command hook.

- Make root-portable native reuse exact for compiler-selected repository files, retain bounded failure telemetry and
  restore synchronization state, and accept rustc's boolean `linker-plugin-lto` spellings. The remote native-object
  contract advances to `native-v6`; old `native-v5` objects remain cleanly unreachable.


## [0.24.0](https://github.com/loadingalias/cargo-rail/compare/v0.23.0...v0.24.0) - 2026-08-28

- Install each CI Rust toolchain into a job-private Rustup home, require Cargo explicitly, and bind downstream steps to
  the verified host-qualified toolchain. This removes runner-image Rustup state from compatibility and release archive
  bootstrap.

- Replaced planner surfaces with the v8 evidence-backed named-work contract, exact Cargo and CI selectors, sparse source capture, and one strict local/CI consumer. Cargo-scoped repository work now inherits exact selectors from subscribed Cargo decisions. Removed the retired classification policy, duplicate affected-work APIs, and planning-only hash, diff-hash, and graph commands.
  Source-checkout consumers now build Cargo-Rail before invoking the binary directly, so plan creation and saved-plan verification observe the same Cargo environment.
  Saved-plan consumers validate one canonical decision and bind it to the exact source checkout without recomputing the planner's Cargo, toolchain, target, or platform identities. Equivalent Cargo home locations no longer change planning identity by path alone.

- Fixed Surface macro spans and target inheritance, added resumable compiler acquisitions and cross-root remote cache
  reuse, and aligned release checks with the local release plan.
  Release packaging now resolves the non-yanked `chacha20` version selected by the Azure SDK dependency chain.

- Preserve verified cross-checkout cache reuse on Windows by remapping the exact checkout spelling observed by rustc.
  Native Windows installer and integration qualification now use platform-correct line endings, file URLs, and path
  construction.


## [0.23.0](https://github.com/loadingalias/cargo-rail/compare/v0.22.2...v0.23.0) - 2026-08-25

- Install authenticated, component-aware Cargo-Rail archives on Apple Silicon macOS, Linux, and Windows. Surface exposes
  a readiness preflight and can build a verified fact driver from bundled offline source for the workspace's exact Rust
  toolchain. Unify avoids compiler views already proven irrelevant, reuses one bounded working set across feature views,
  reclaims completed targets, scales its free-space reserve to the physical volume, and stops on the first required Cargo
  failure. Native caching now supports deterministic policy flags and qualified target roots, records early bypass
  reasons, cleans process state, and safely quarantines markerless local stores. Release planning defaults registry
  publication off, preserves Cargo registry restrictions, reconciles completed prepare transactions, and keeps generic
  workspace cleanup away from the shared compiler cache. Native archives manufacture byte-identical offline driver source
  across host newline conventions and qualify the executable compiler capability instead of Rustup package inventory.
  Runtime driver preparation distinguishes residual compiler runtime libraries from the development metadata required to
  build against a selected toolchain. Runtime driver capture accepts Cargo's hard-linked build output before restaging it
  as private cache authority. Windows archive qualification binds dated-nightly Surface checks to exact compiler tools
  instead of ambient `PATH` selection. Effective configuration now prints as strict round-trip input.

- Raise the public MSRV to Rust 1.98, enforce its new FFI and runtime-symbol diagnostics, keep release arithmetic checked,
  pin CI tools exactly, and run doctests in full local and release-quality test lanes.


## [0.22.2](https://github.com/loadingalias/cargo-rail/compare/v0.22.1...v0.22.2) - 2026-08-24

- Update Cargo-Rail's public GitHub Actions examples and dogfood workflow to use cargo-rail-action v7 with Cargo-Rail
  0.22.1.

- Preserve every benchmark compiler-coverage event when parallel rustc wrappers select the same initial event filename
  instead of failing the compilation with an `EEXIST` error.

## [0.22.1](https://github.com/loadingalias/cargo-rail/compare/v0.22.0...v0.22.1) - 2026-08-24

- Shorten the README around the product decision, safe evaluation, installation, and adoption. Remove stale
  version-pinned launch copy and move deep operational detail behind maintained references.

- Preserve Rustup proxy selection while binding distributed execution to the exact sysroot compiler, derive archive
  protocol checks from the worker itself, and require the same eight-target archive build and smoke-test gate before a
  release commit can become publishable.

- Resolve release-archive executables from an absolute extraction root so smoke tests remain valid after changing their
  working directory, and preserve the failing diagnostic when an archive violates its Surface capability contract.

## [0.22.0](https://github.com/loadingalias/cargo-rail/compare/v0.21.0...v0.22.0) - 2026-08-23

- Make compiler-cache compatibility validation fail explicitly when benchmark evidence cannot be recorded, and restore
  Windows builds after opened-file generation became Unix-only.

- Expanded transparent compiler reuse to every exact compiler-owned Rust result supported by the current native
  platform witness: metadata and rlib outputs, compiler-owned static archives, build-script and proc-macro producers,
  ordinary binaries, tests, examples, benchmarks, dylibs, and cdylibs. Apple and Linux ELF linking share one typed
  linker witness; COFF, explicit linkers, cross targets, incremental work products, alternate compilers, external
  backends, Clippy diagnostics, rustdoc output, doctest execution, proc-macro consumers, and build-script execution keep
  specific acquisition-free cold boundaries when complete authority is unavailable.

  Added a root-independent operation inventory for Rust compilation, build-script execution, native compilation,
  compiler-owned `asm!`, `global_asm!`, `naked_asm!`, included and target-feature-sensitive assembly results,
  Rust-required external assembly, preprocessing, archives, probes, generated outputs, and downstream artifact edges.
  Path-bearing and opaque compiler values retain typed shapes without embedding checkout roots in comparison identities;
  raw compiler arguments remain in retained evidence.
  Benchmark qualification now rejects ambiguous or unaccounted work, unsafe competitor hits, incomplete requested
  outputs, and compiler modes that touch cache or remote state. IBM Power, IBM Z, and RISC-V claims remain blocked on
  their exact hardware-access evidence gates. Local shared-hit superiority requires at least a 10% p50 and p95 reduction
  against the pinned sccache lane, records the separate 15% target, and cannot be hidden by the aggregate overhead result.
  Real-provider correctness now covers check, release build, and all-target test compilation and requires a retained
  producer/consumer pair report proving exact root-independent action multisets, output bytes, read-only import, and
  offline L1 behavior.
  Provider fault qualification now shares one AWS S3, Azure Blob, and Cloudflare R2 harness with dedicated
  run namespaces. Each backend must reject a corrupt object, fall back when that object is absent, and build through a
  network-denied outage without changing exact outputs or writing remotely. Cleanup rejects broader authorities; Azure
  qualification requires and removes one dedicated run container.
  Native host qualification now binds the exact host and filesystem to the compatibility corpus, authenticated full
  suite, doctests, complete cache coverage, and the retained performance decision on Linux and Windows x64/Arm64. Its
  dedicated bootstrap profile installs both the full validation toolchain and the pinned benchmark tools.

- Established one deterministic compiler-fact analysis scheduler over captured manifest and source inputs. Stable
  diagnostic collection now consumes scheduler-owned Cargo views and fixed arguments, and fact-required workspace
  compilations bypass ordinary compiler-result reuse unless a later combined protocol proves the required sidecar.
  Incomplete fact runs are no longer reusable. Removed the superseded public `AnalysisConfiguration` representation and
  the public collector constructor that could recapture workspace state independently. Added the bounded canonical
  typed-fact fragment protocol, including exact run/compiler/driver/unit authority, root-independent source identities,
  byte-bounded definition and visibility spans, entry points, typed edges, conservative roots, and explicit completeness
  coverage. Typed and diagnostic requirements now collapse only identical Cargo checks while compile-only doctests remain
  separate. Authenticated release drivers are exact-toolchain sibling components with guarded execution paths on Linux
  and Windows; source installations perform no driver discovery. Fact sidecars are admitted only through canonical
  content-addressed compiler-message announcements and are revalidated before use. Exact run-independent fact objects
  and complete view manifests now reuse the shared local CAS across moved workspace roots; missing, corrupt, partial, or
  authority-mismatched sets remain misses. A clean-root production-collector workload proves one combined cold schedule
  uses half the Cargo views of independent diagnostics and typed collectors, then reopens identical facts with zero Cargo
  views after removing the driver executable. Native compatibility jobs compile and execute the matched corpus on every
  supported Linux and Windows host, and release archive smoke tests load the bundled driver against its exact
  authenticated rustc runtime.

- Consolidated compiler cache, rustc observation, and rustdoc proxy execution behind one exact pre-Clap invocation
  boundary. Ambiguous roles now fail before workspace acquisition, analysis facts require a private run capability, and
  disabled or clearly unsupported compiler shapes execute the original chain before session or CAS loading. Shared CAS
  and output-manifest ownership moved out of the whole-action runner boundary.

- Compiler-observation storage and acquisition failures now stop `unify` as operational errors instead of continuing
  with graph-only analysis. Resource failures can no longer produce plausible but unsupported unused-dependency or
  feature verdicts.

- Added optional distributed execution for a deliberately bounded class of compiler-owned Rust operations. Requests use
  a versioned typed protocol with canonical source namespaces, exact Rust dependency inputs, fixed compiler options, and
  metadata or library outputs. Unsupported inputs, native or dynamic dependencies, linking, incomplete environment
  evidence, and unknown compiler shapes stay local.

  `cache setup` can pin one mutually authenticated worker and its exact compiler, sysroot, platform, endpoint, trust
  root, client identity, and execution policy. The qualified Linux policy runs each attempt in an empty-root Bubblewrap
  sandbox with private namespaces and an exact cgroup-v2 envelope for CPU, memory, processes, scratch space, time,
  streams, and outputs. Startup probes require observed CPU throttling, an OOM kill, process-limit enforcement, and an
  idle hierarchy before the worker accepts normal work. The direct worker remains for dedicated single-tenant or
  ephemeral machines; it is not a multi-tenant service or general remote runner.

  Distribution runs only after local L1 and remote L2 miss. Transport, protocol, capability, lease, and pre-effect worker
  failures fall back to the exact local compiler command. Compiler failures retain their exit state and bounded
  diagnostics without returning partial artifacts. Successful responses enter private staging and must pass the same
  live recapture, action/result verification, and atomic restore transaction as local cache results before any output is
  published. Workers never receive cache-provider credentials or cache write authority.

  Automatic placement uses bounded, expiring, source-free cost history and delegates only when at least three local and
  remote observations predict a critical-path win. In the retained same-shape `c8i.large` qualification, a six-crate
  dependency DAG completed in 10.098 seconds p50: 29.57% below local Cargo and 28.84% below pinned distributed sccache.
  Small, single-large-unit, and parallel-check workloads lost and remain local, so this result is intentionally limited
  to the qualified dependency-DAG topology.

- Corrected distributed timing qualification to validate client and worker intervals independently when network source
  transfer and worker execution overlap.

- Fixed GitHub repository detection for remote URLs with a trailing slash, and rejected non-repository paths before
  they could produce incorrect changelog or release links. Release transactions now bind the one effective origin
  repository shared by fetch and push operations, persist it for recovery, and target forge commands explicitly.

- Bound dependency coherence to one captured workspace graph. Root and member manifests, inherited workspace
  dependencies, source feature evidence, conservative documentation references, and MSRV policy now come from the same
  snapshot. Existing inherited declarations participate in unused-dependency proof without producing no-op edits, and
  renamed dependencies retain their exact Cargo alias and package identity through planning and application.

  Captured `[workspace.package]` policy now produces explicit inheritance decisions. Unify rewrites only member values
  that are semantically equal and safe to inherit, reports missing and divergent declarations without changing them, and
  retains version- and workspace-relative path fields for their owning release or path policy. JSON, explanations,
  Markdown reports, proof certificates, mutation traces, previews, receipts, and deterministic apply order share the same
  decision set.

  Added root-independent, versioned feature/target coverage views with direct Cargo and nextest argument arrays. Removed
  the former target-load result that was presented as validation despite proving only that already-required metadata
  existed. Each target now carries only feature selections whose captured cfg predicates can apply to that target.
  Dependency rust-version metadata is now reported as an unprobed lower-bound candidate and cannot silently
  lower a higher declared workspace MSRV. The report separately records whether the captured compiler satisfies that
  candidate and states that no candidate-compiler build authorized a lowering. Apply reconstructs every captured
  feature/target view as an exact Cargo resolution, restores all manifests if any view fails, and records the verified
  view identities in its receipt and machine output. Cargo lockfile refresh is an explicit planned mutation with backup,
  fingerprint, rollback, and undo coverage. Manifest, report, and receipt writes are atomic, reports are deterministic,
  and any write or verification failure restores the complete authorized file set. Post-apply validation retains
  fingerprint-bound starting changes while rejecting newly unplanned paths; commands such as release that prohibit
  unrelated starting dirt retain their stricter boundary. Public workspace-hack replacement claims were removed until
  generated-hack detection, exact removal, and end-to-end parity evidence exist.

- Made the serialized planner the complete Cargo package-scope authority. Planner contract v7 and scope contract v4 use
  one declared dependency universe across optional features, target predicates, and dependency kinds. Every
  package-scoped surface now contains its final Cargo argument array for direct use by Cargo, cargo-nextest, Just, and
  CI. GitHub output also includes a deterministic `surfaces_json` projection for bounded job routing.

- Corrected public planner and compiler-cache integration examples, pinned GitHub Actions to immutable revisions, and
  aligned command help, cache status, remote-environment documentation, and the generated support matrix with the
  executable contracts. Removed an obsolete internal campaign ledger that contradicted the supported-host and provider
  records and could not represent completed evidence.

- Finalizing a merged release pull request now tags the already-proven merge commit without creating or pushing an empty
  commit or updating the protected branch. Recovery also recognizes transactions left by older versions that created a
  legacy finalize commit.

- Replaced repository-selected `[cache]` aliases with one strict machine-owned remote URL authority. Setup now persists
  explicit read or read-write AWS S3, Azure Blob Storage, and Cloudflare R2 destinations, can return to local-only mode,
  and reports only redacted authority. A new network-free normalization command validates provider URLs before
  credentials or storage are consulted. Existing repository `[cache]` configuration is rejected with migration guidance.

  Added transparent result sharing through one conditional object protocol and a private bounded coordinator that reuses
  provider credentials, clients, and connections without retaining build results in memory. Verified local packed results
  remain authoritative and network-free; absence, conflict, corruption, credential failure, coordinator failure, or
  provider outage falls back to exact cold compilation. Qualified Linux ELF linker evidence also expands safe reuse to
  linked build-script, proc-macro, and final executable outputs.

  On the retained Linux x64 corpus, local L1 was 77.55–89.25% faster than pinned sccache at p95 while restoring more
  compiler actions. The empty-L1 AWS S3 corpus was 43.58% faster for check and 73.27% faster for release build at p95.
  Azure Blob and R2 passed independent live producer/consumer, read-only, offline-L1, corruption/outage, and cleanup
  qualification; these results do not claim Azure or R2 performance superiority.

- Removed the top-level `cargo rail run` command and `[run]` configuration. The planner's versioned per-surface Cargo
  arguments are now the only workspace-scope handoff; Cargo, cargo-nextest, Just, and CI execute that scope directly.
  Runner-owned profiles, workflows, actions, hermetic command wrapping, whole-action caching, and dry-run projections are
  gone. Transparent verified compiler reuse remains available beneath ordinary Cargo invocations.

- Added complete workspace Rust source-surface analysis from authenticated compiler facts. `cargo rail surface --check`
  separates production, non-production, and required-public reachability across configured targets and reports dead or
  unnecessarily broad visibility only for closed compiler crates while preserving every open target observation.
  Versioned text, JSON, GitHub, schema, reason, fragment, and cache evidence share one deterministic report.

  Added exact visibility repair with snapshot-bound byte spans, deterministic mutation plans, drift rejection, bounded
  backups, atomic file replacement, rollback, receipts, and post-write recompilation of every configured view. Public
  declaration deletion remains report-only. Planner contract v7 adds a whole-workspace `surface` decision without
  claiming package-scoped Cargo arguments.

  `[surface] enabled = true` opts the planner and CI into that gate. `cargo rail surface` without an operation flag is a
  read-only, non-failing report; only explicit `--fix` grants source-write authority.

  Documented native `[surface]` adoption, closed-world limits, planner routing, and a reproducible benchmarking contract
  without claiming an unsupported universal wall-time multiplier.

- Made Surface analysis installable and CI-native through `[surface] enabled`, planner-selected commit gating, native
  release companions, and archive smoke tests. Supported release archives now execute a versioned Surface report, while
  unsupported targets fail with an explicit availability diagnostic.

  Exact warm compiler-fact reuse now avoids Cargo and rustc on an unchanged workspace, expands ordinary response files
  into the observed and executed argument stream, and bypasses forms it cannot model exactly. Compiler-target evidence
  and retained runtime generations keep cache hits, findings, and visibility repairs fail-closed on incomplete authority.

- Added one safe machine setup for daemonless verified local compiler reuse beneath ordinary Cargo, nextest, Just, IDE,
  CI, and Cargo-Rail commands. Cargo freshness and incremental compilation remain authoritative; disabled, incremental,
  ambiguous-wrapper, and unsupported compiler shapes bypass before session or cache acquisition. Setup, status, repair,
  opt-out, cleanup, and exact receipt-owned removal share one private bounded local authority. A minimal launcher
  preserves the disabled compiler contract without starting the receipt-authenticated cache worker. Metadata/rlib actions,
  including metadata-only proc-macro producers, exact native-static consumers, and certified Apple build-script
  executables, proc-macro producer dylibs, and final linked artifacts use one action/witness/result pack with verified
  atomic L1 restore. Native proc-macro consumers remain cold.
  Removed runner-owned cache activation and transfer rather than maintaining a second cache protocol. Optional
  machine-owned remote authorities transport the same exact compiler-owned result pack used by the local cache.
  On the canonical five-sample local fixture, verified warm L1 was strictly faster than pinned sccache at p50 and p95 for
  both check and release workloads while safely reusing more compiler actions and rejecting unsafe native proc-macro
  consumer hits.

- Repair missing local compiler-cache storage without replacing unchanged installation files, avoiding transient Windows
  executable-lock failures while retaining exact per-executable plan/apply drift validation.

- Retain typed compiler-fact doctest staging across Windows NTFS volumes with digest-authenticated private executable
  copies and a handle-guarded sysroot junction, replacing per-file sysroot hard-link mirroring without weakening drift
  rejection.

- Retry Windows compiler-sysroot fingerprinting after transient NTFS metadata drift while requiring one complete stable
  rehash before cache reuse.

## [0.21.0](https://github.com/loadingalias/cargo-rail/compare/v0.20.1...v0.21.0) - 2026-08-03

- Add automatic verified compiler reuse for eligible clean Cargo profiles while preserving active incremental profiles,
  explicit incremental requests, and existing wrappers. Move compiler evidence into the typed, user-wide
  content-addressed store and retain the bounded legacy file only for one-time import.

  Content-identify each exact Cargo, rustc, rustdoc, complete sysroot, host, environment, and compiler invocation instead
  of gating native reuse through a checked-in toolchain allowlist. Structurally eligible native library units can reuse
  verified results across supported Rust releases and bundled named codegen backends; external backend paths and
  incompletely modeled units continue through the exact compiler invocation.

  Add separately scoped workspace/local cache status, preview, and cleanup commands while keeping `clean --cache` as the
  combined compatibility alias. Coordinate restore, publication, garbage collection, status, and cleanup through one
  validated lifecycle lock; reclaim crash staging and evict least-recently-used unleased results under the configured
  byte bound. Local status and cleanup remain exact after `cargo clean` without a workspace-local pointer to user-wide
  state. Keep Windows lifecycle locks buildable on stable Rust while preserving exact file identity, hard-link rejection,
  and pathname replacement resistance.

  Delegate the exact normal all-workspace build and distribution actions directly to unchanged Cargo when an active
  profile and its target location are statically unambiguous. This preserves Cargo and wrapper ownership while avoiding
  metadata, Git, tool hashing, action-key construction, and cache setup. For an eligible clean profile, prove the
  workspace-library boundary with one locked, no-dependencies Cargo metadata query and enter verified compiler reuse
  without capturing Git state or expanding the full action plan; ambiguous and unsupported shapes retain captured
  planning.
  Default text execution reports one concise native-cache decision; `--explain` retains the full stable reason and
  per-unit evidence plus accounted verified-result bytes read and written. Stop synchronously flushing observational
  run receipts and parent-owned compiler observations because they are not recovery authority.

  Memoize the complete Linux sysroot identity only while two exact filesystem inventories agree, and bypass to a full
  hash when generation evidence changes. Reuse non-Rust files reported by rustc dep-info, including `include!`,
  `include_str!`, and `include_bytes!` inputs. Keep timed benchmark commands free of opt-in diagnostics and validate their
  outcomes through a separate unmeasured replay. The benchmark workflow now defaults to one accepted interleaved group
  and treats that complete group as publishable bounded evidence; larger sample counts are explicit for distribution or
  tail claims.

  Move native compiler identity from whole-workspace session state to the exact compiler unit that consumed it. The
  session no longer includes the complete Cargo configuration or `Cargo.lock`; exact rustc arguments, cfg, sources,
  dependency contents, observed filesystem reads, observed environment reads, and a narrow compiler-process environment
  remain authoritative. Output-neutral warning, job, build-directory, target-directory, network, registry, and
  unrelated lockfile changes can now reuse verified results, while unknown or unobservable behavior continues to bypass.

  Preserve Cargo's exact rustc argv and current directory. Bind opaque Rust metadata to the physical source root instead
  of rewriting the compiler invocation and claiming unsafe cross-checkout portability. Store reversible internal path
  tokens for verified dep-info and JSON compiler streams, then late-bind the current output directory after CAS
  verification while preserving Windows separators and escaping exactly. Remove the redundant pre-context lockfile hash,
  warm rehashing of already captured inputs, and the cold-path dep-info root scan. Strengthen compatibility and benchmark
  oracles to compare direct, disabled, cold, and warm output bytes without root-bound exclusions, and invalidate
  qualification evidence when the cache execution contract changes. Use the public MSRV as the repository and CI
  toolchain, preserving MSRV-native host, filesystem, cross-target, linker, and cache-correctness coverage without
  separate live stable, intermediate stable, beta, or nightly release gates. Also make the benchmark identity manifest
  valid when the repository has no untracked files.

  Decode rustc's Makefile-escaped dep-info environment values before hashing them, so Windows `OUT_DIR` and Cargo path
  dependencies compare against the exact compiler process environment instead of producing false warm misses. Require
  every cold cache publication to become a warm hit in the real-world fixture while preserving the exact bypass count.
  Run that long fixture first and exclusively under nextest so Windows load cannot consume its bounded timeout.

  Retain reviewed Rust 1.97.1 Linux and Windows v4 qualification corpora with 20 accepted lane samples, two complete
  groups, no rejections, and no false hits on each host. Keep large benchmark diagnostics file-backed through `jq`
  instead of passing megabyte JSON values through the process argument limit.

## [0.20.1](https://github.com/loadingalias/cargo-rail/compare/v0.20.0...v0.20.1) - 2026-07-31

- Consolidate planning and caching documentation, correct the release-source example, and refresh Rust dependencies and
  CI action pins.

## [0.20.0](https://github.com/loadingalias/cargo-rail/compare/v0.19.1...v0.20.0) - 2026-07-29

- Raised the repository and package toolchain to Rust 1.97.1, updated Rust dependencies, the native-cache fixture
  dependency graph, GitHub Actions, the CI planner's cargo-rail version, and the native-cache comparator's sccache
  installation to their current releases. Release rebuilds now use the exact package toolchain, immutable assets are
  verified instead of overwritten, and action verification binds every workflow pin to the action lock.

- Prevented bulk Git object reads from deadlocking when request and response pipes fill.

- Hardened backup, release, split, and sync mutation boundaries against path escape, symlink traversal, cross-operation
  plan reuse, and exact-release checkout drift. Split and sync check modes now distinguish clean state from pending work,
  configuration validation rejects malformed unify globs and empty split branches, and release checks report skipped
  evidence separately from passed checks.

- Added exact generated native-cache capability authority and `cargo rail doctor native-cache`, kept uncertified hosts on
  stable fail-closed bypasses, hardened Windows cache boundaries against reparse points and transient reader conflicts,
  and preserved Cargo's workspace path spelling so Windows compiler outputs remain byte-exact.

- Published one generated execution, cache, and performance support matrix, added continuous full-suite CI for all six
  advertised native host/architecture pairs, and added macOS x86-64 plus required Linux musl release inventories.

- Presented the `cargo-rail` crate and CLI as Cargo-Rail, the Rust workspace engine, and aligned the README, CLI help,
  package metadata, and public documentation around its shared Cargo and Git decision model without renaming technical
  interfaces.

## [0.19.1](https://github.com/loadingalias/cargo-rail/compare/v0.19.0...v0.19.1) - 2026-07-25

- Hardened exact-SHA release readiness to reject all-skipped GitHub rollups and run release commits through normal CI. `cargo rail config migrate` now removes the inert `release.require_clean` and `release.publish_delay` fields, and release previews no longer claim to delay between publishes. Added explicit cache capability and evaluation guidance.

## [0.19.0](https://github.com/loadingalias/cargo-rail/compare/v0.18.0...v0.19.0) - 2026-07-24

- Make split and sync snapshot-native by replacing path ownership with Cargo member names, persisting versioned
  `Rail-Origin` provenance in ordinary Git history, migrating legacy notes, preserving exact Git trees and commit
  metadata, and binding planner and release output to the shared snapshot.

- Add fail-closed action-key diagnostics over exact source, resolution, toolchain, executable, Cargo configuration, argv,
  typed environment, and verified dependency-result identities. Transparent rustdoc observation preserves the selected
  tool and HTML output while recording stable dep-info. Build-script compilation separates its non-circular
  pre-execution action identity from the ordered instructions, environment reads, generated tree, and execution evidence
  in its result identity. Incomplete boundaries remain explicitly non-reusable while ordinary unsupported execution stays
  available.

  Add `cargo rail run --all --action build --hermetic` for the graduated pure-Rust Cargo-check class. It performs an
  explicit locked fetch, captures immutable crates.io, remote registry-mirror, or Git dependency sources, then checks
  locked/offline in fresh read-only source and isolated output roots with logical path remapping and a controlled
  environment. macOS enforces filesystem and network denial and can issue a verified action/result manifest; other hosts
  remain platform-limited. Build scripts, proc macros, docs, linked/native/cross-target work, custom tool boundaries, and
  sccache fail closed. Cargo fingerprints and incremental state are never restored. Action plans and decision receipts
  use schema version 4.

- Add a bounded machine-local action/output cache for eligible macOS hermetic Cargo checks. Verified hits restore exact
  declared outputs into a clean root without starting Cargo or rustc; changed inputs, corrupt objects, unsupported
  classes, and other platforms remain fail-closed. Add `--no-cache` and extend `run --explain`, diagnostics, and
  `clean --cache` with local-cache decisions.

- Add portable, verified native compiler-result caching for non-incremental dependency and workspace library
  metadata/rlib units on Apple Silicon macOS and ARM64 Linux with Cargo/rustc 1.97.1. Ordinary `cargo rail run` check
  and build actions can reuse byte-exact outputs across clean roots without restoring or fabricating Cargo target state,
  incremental state, or fingerprints.

  Preserve custom wrappers and sccache, keep incremental builds and unproven linker/build-script/proc-macro classes
  explicitly bypassed, and fail closed on input, toolchain, environment, SDK/linker, cache-object, and output mutations.
  Add a representative registry/Git/native/proc-macro fixture plus reproducible cold/warm benchmarks and cache evidence.

- Make reviewed change files authoritative for release planning and add exact-SHA, resumable, tags-last release execution.

- Make planner impact semantic and target-aware, bind run actions to exact Cargo resolution views, and add explainable dependency-unification diagnostics.

## [0.18.0](https://github.com/loadingalias/cargo-rail/compare/v0.17.3...v0.18.0) - 2026-07-19

- Capture complete, stable source state for deterministic planning from Git worktrees or declared Cargo filesystem roots; reject concurrent Git, byte, directory, or metadata drift; keep historical ranges object-only; support nested and no-Git Cargo workspaces; and exclude resolved Cargo and cargo-rail generated state.

- Preserve every Cargo package as an exact `PackageId`-keyed graph node and build dependency edges from Cargo's resolved graph, retaining distinct versions, renamed dependencies, dependency kinds, and target conditions while keeping inactive declarations out of resolved topology and confining package-name lookup to ambiguity-aware workspace selection.

- Add lazy exact Cargo resolution views keyed by package, feature, target, toolchain, and sanitized Cargo configuration; replace filename heuristics with deterministic PackageId ownership; and introduce opt-in immutable workspace snapshots over exact source, manifests, lockfile, configuration, toolchain, and target inputs without slowing native/default commands.

- Replace hard-coded run surfaces with a bounded, snapshot-bound action graph. Built-in and repository actions now share
  one shell-free expansion and stable topological order across local execution, JSON/GitHub CI plans, and version-2
  decision receipts. Repository generators declare exclusive outputs plus separate check/regenerate commands; paths,
  dependencies, tokens, environment capabilities, cycles, and portable ownership collisions fail closed before
  execution. Ownership validation remains fast at the configured action/path limits, and command startup retains safe
  stack headroom on Windows as the action CLI grows.

- Make rail.toml sparse, add config explain and semantic migrations, and replace invalid option matrices with typed policies.

### BREAKING CHANGES

- **run**: [**breaking**] replace surfaces with a bounded action graph ([71a5972](https://github.com/loadingalias/cargo-rail/commit/71a5972c26388b286dc10175101a6a7100e36af3))
- **config**: [**breaking**] make repository policy sparse ([577f55b](https://github.com/loadingalias/cargo-rail/commit/577f55b1a9b520c69927a3f08adc726c6ea2ecf0))

### Features

- **workspace**: bind commands to canonical snapshots ([7b70568](https://github.com/loadingalias/cargo-rail/commit/7b70568a6d450988299e5cc93ffaf7b354ca1a7b))
- **workspace**: establish exact resolution snapshots ([fec7444](https://github.com/loadingalias/cargo-rail/commit/fec7444ae5cc10ffb393cc4e515ea942e716dbb1))
- **workspace**: stabilize source and package identity ([5399da4](https://github.com/loadingalias/cargo-rail/commit/5399da4e5361a9cad613ade6338f732c3b6f0650))
- **planner**: capture complete worktree source state ([9de889f](https://github.com/loadingalias/cargo-rail/commit/9de889f9f417c79d6aa5cc5e273414eb8eb50905))

### Bug Fixes

- **cli**: prevent Windows startup stack overflow ([2726bd9](https://github.com/loadingalias/cargo-rail/commit/2726bd9fb468938a598b1e171064350730334016))
- **workspace**: report directory drift deterministically ([3c99742](https://github.com/loadingalias/cargo-rail/commit/3c997426cfe87037e93735be4a3ca62952eff698))
- **workspace**: normalize snapshot paths on Windows ([dfe18d4](https://github.com/loadingalias/cargo-rail/commit/dfe18d404684ec1d582cd72de002953b96c72a0e))

## [0.17.3](https://github.com/loadingalias/cargo-rail/compare/v0.17.2...v0.17.3) - 2026-07-14

- Fixed crates.io publication checks so local workspace packages cannot masquerade as published versions. Release publishing now targets crates.io explicitly, requires the committed lockfile, rejects dirty package contents, and excludes Finder metadata.

### Bug Fixes

- **release**: verify crates.io publication explicitly ([1bd5c68](https://github.com/loadingalias/cargo-rail/commit/1bd5c68efe44ca4e9c39616bae1f568a5d11d20d))

## [0.17.2](https://github.com/loadingalias/cargo-rail/compare/v0.17.1...v0.17.2) - 2026-07-14

- Fixed release Git operations to preserve the caller environment for hooks, expose standard cargo-rail release context, and retain complete hook diagnostics. Removed the hook-bypassing push dry run while keeping one atomic branch-and-tag push.

### Bug Fixes

- **release**: preserve hook context and diagnostics ([61da35d](https://github.com/loadingalias/cargo-rail/commit/61da35d4da0964618d95d0de2031a6516003bf84))

## [0.17.1](https://github.com/loadingalias/cargo-rail/compare/v0.17.0...v0.17.1) - 2026-07-12

- Fixed unify graph verification to compare pre- and post-edit metadata with the same target platform filter. Cargo-synthesized optional-dependency features are no longer treated as writable manifest feature keys.

- Allowed release abort to reconcile an atomic push rejected before any remote refs changed. Increased the strict nextest leak deadline to avoid false failures from loaded macOS process teardown while continuing to fail persistent inherited-process leaks.

- Kept immutable release recovery from being blocked by Clippy lints added after a tag was published. Normal tag-triggered releases still require a clean Clippy run.

- Synchronized upgrade policy, aligned CI and examples with cargo-rail-action v5.1.0, removed deprecated Intel macOS distribution, tightened dependency checks, and reduced CI duplication while preserving Linux, Windows, ARM, MSRV, and cross-OS test coverage.

### Bug Fixes

- **release**: recover locally rejected atomic pushes ([7340378](https://github.com/loadingalias/cargo-rail/commit/73403782bdf8921097c168f6911e2b3f00947d50))
- **workspace**: harden graph cleanup and release readiness ([3c8b7d4](https://github.com/loadingalias/cargo-rail/commit/3c8b7d410d12701c4207ee4e745578c34a5371c0))
- **release**: recover immutable tags from lint drift ([a10f176](https://github.com/loadingalias/cargo-rail/commit/a10f1763e3d0d54f0c982880304913dcd3d24808))

## [0.17.0](https://github.com/loadingalias/cargo-rail/compare/v0.16.0...v0.17.0) - 2026-07-12

- Made `cargo rail unify` faster and more exact with shared indexed Cargo metadata, workspace-only compiler evidence, source-derived feature checks, and compilation-unit cache reuse. Analysis now covers configured targets, default/no-default/all-feature builds, conditional feature selections, generated and macro-expanded source, every Cargo target kind, and target-scoped declarations.

  Graph-removing decisions now carry deterministic proof certificates with repository-relative paths normalized across platforms. Apply verifies the exact declaration edits and resulting portable Cargo graph before writing. Closed-world cleanup of dormant private features and optional dependencies requires the explicit `consumer_scope = "workspace"` contract; published feature APIs remain preserved.

- Fixed release archive verification and added recovery for an existing immutable tag.

- Restored the changelog introduction, preserved it above future releases, updated dependencies and CI action pins, and documented unavoidable duplicate graph dependencies.

### Features

- **unify**: add compiler-backed graph cleanup ([74ae271](https://github.com/loadingalias/cargo-rail/commit/74ae27107f4325a59a2010fe70333647da19fd07))

### Bug Fixes

- **unify**: normalize proof paths on Windows ([eee5446](https://github.com/loadingalias/cargo-rail/commit/eee54464d5779c0389e36680c7ce1976249456b4))
- **release**: finish patch release housekeeping ([9c355e3](https://github.com/loadingalias/cargo-rail/commit/9c355e30bc9d73de1c244e44c780cf60d29e28be))
- **release**: recover immutable release assets ([5608728](https://github.com/loadingalias/cargo-rail/commit/56087284d869818f6d37713ff4e6cc8e2722280d))

This file records user-visible changes. Git tags and [GitHub Releases](https://github.com/loadingalias/cargo-rail/releases) retain the complete release history.

## [0.16.0](https://github.com/loadingalias/cargo-rail/compare/v0.15.0...v0.16.0) - 2026-07-11

- Added Cargo-ready planner scope args, automation-safe change status output, and a commit-time change-file coverage check.

- Changed public Rust APIs for mutation contracts, release execution, split/sync safety, and test-runner selection; downstream library users must update constructors and method calls.

- Made command output formats exact, published the planner v3 JSON Schema, and added checkout-independent plan identities.

- Curated the historical changelog and required reviewed release intent for future releases.

- Bounded release, split, and sync mutations to approved repository paths, made sync conflicts resumable, and preserved exact split history and mappings.

- Skipped crates.io preflight checks when every crate in a release plan has publishing disabled.

- Made releases resumable, verified and distributed the exact tagged commit, and made Cargo, nextest, filter, and test-harness arguments backend-correct.

- Fixed Windows path normalization for release, split, sync, and portable planner identities.

### Features

- **workspace**: make control-plane operations verifiable and recoverable ([6bd64fb](https://github.com/loadingalias/cargo-rail/commit/6bd64fb6f028bee13a372ff23d4f4b789a5562b3))

### Bug Fixes

- **release**: harden release readiness and curate history ([3679801](https://github.com/loadingalias/cargo-rail/commit/367980186587210735bbecf9a7b6e3485cf2985b))
- **git**: normalize Windows paths at repository boundaries ([7936232](https://github.com/loadingalias/cargo-rail/commit/793623232c23e25d2f92734ca673421052c40b4a))

### Documentation

- **release**: record v0.16 library API breaks ([3595b4f](https://github.com/loadingalias/cargo-rail/commit/3595b4f1f4c0e6c4f89f4b66688a597bbad0f61b))

## [0.15.0](https://github.com/loadingalias/cargo-rail/compare/v0.14.0...v0.15.0) - 2026-07-06

### Added

- Added the built-in changelog engine used by the release workflow.

### Fixed

- Made change-file path assertions portable across operating systems.

## [0.14.0](https://github.com/loadingalias/cargo-rail/compare/v0.13.4...v0.14.0) - 2026-07-06

### Added

- Added graph-aware commit attribution for per-crate changelogs.

## [0.13.4](https://github.com/loadingalias/cargo-rail/compare/v0.13.3...v0.13.4) - 2026-06-01

### Fixed

- Prevented release dry runs from invoking pre-push hooks.

## [0.13.3](https://github.com/loadingalias/cargo-rail/compare/v0.13.2...v0.13.3) - 2026-06-01

### Fixed

- Made release CI wait for cargo-rail to create the GitHub Release before uploading assets.

## [0.13.2](https://github.com/loadingalias/cargo-rail/compare/v0.13.1...v0.13.2) - 2026-06-01

### Added

- Added the end-to-end publishing lane for release commits, tags, forge releases, and crates.io publication.

## [0.13.1](https://github.com/loadingalias/cargo-rail/compare/v0.13.0...v0.13.1) - 2026-05-21

### Fixed

- Allowed `cargo rail unify --check` to run outside a Git repository.

## [0.13.0](https://github.com/loadingalias/cargo-rail/compare/v0.12.0...v0.13.0) - 2026-04-18

### Changed

- Finalized planner scope semantics and raised the MSRV to Rust 1.95.
- Added per-dependency unification decisions and stricter action contract validation.
- Unified change detection under the planner surface taxonomy.

## [0.12.0](https://github.com/loadingalias/cargo-rail/compare/v0.11.0...v0.12.0) - 2026-04-17

### Changed

- Made custom planner surfaces additive instead of replacing built-in classifications.
- Added bounded summaries for plans affecting large crate sets.

### Fixed

- Updated cargo-rail-action compatibility and pinned its workflow reference.

## [0.11.0](https://github.com/loadingalias/cargo-rail/compare/v0.10.12...v0.11.0) - 2026-04-09

### Added

- Added stable execution scope and ready-to-pass Cargo package arguments for CI consumers.

### Fixed

- Isolated the bootstrap target directory on Windows.
- Normalized workspace paths in cross-platform tests.

## Historical releases

Releases before `0.11.0` were generated from raw commit subjects. The table keeps the user-facing milestones while the linked comparisons preserve exact history.

| Series                                                                           | Dates                    | User-visible milestones                                                                                                                                                                                |
| -------------------------------------------------------------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`0.10.x`](https://github.com/loadingalias/cargo-rail/compare/v0.9.1...v0.10.12) | 2026-02-14 to 2026-02-19 | Replaced `affected`/`test` with `plan`/`run`; added workspace-member cohort safety, compiler-backed unused-dependency detection, and multi-target fixes. `0.10.11` contained only release bookkeeping. |
| [`0.9.x`](https://github.com/loadingalias/cargo-rail/compare/v0.8.1...v0.9.1)    | 2026-02-03 to 2026-02-10 | Added binary-crate filtering, metadata-cache invalidation, check-mode output files, and release checksums.                                                                                             |
| [`0.8.x`](https://github.com/loadingalias/cargo-rail/compare/v0.7.3...v0.8.1)    | 2025-12-18               | Added MSRV inheritance, workspace lint integration, and optional-feature check-mode fixes.                                                                                                             |
| [`0.7.x`](https://github.com/loadingalias/cargo-rail/compare/v0.6.0...v0.7.3)    | 2025-12-14 to 2025-12-16 | Added configurable dependency sorting and corrected CI and release behavior.                                                                                                                           |
| [`0.6.0`](https://github.com/loadingalias/cargo-rail/compare/v0.5.3...v0.6.0)    | 2025-12-14               | Hardened split/sync, removed production panics, and revised CLI and configuration output. This version had a GitHub Release but was not published to crates.io.                                        |
| [`0.5.x`](https://github.com/loadingalias/cargo-rail/compare/v0.4.2...v0.5.3)    | 2025-12-12               | Added borrowed-feature detection and repair, removed cargo-udeps, and corrected MSRV handling.                                                                                                         |
| [`0.4.x`](https://github.com/loadingalias/cargo-rail/compare/v0.3.0...v0.4.2)    | 2025-12-11               | Added configuration synchronization and fixed release lockfile handling and target matching.                                                                                                           |
| [`0.3.0`](https://github.com/loadingalias/cargo-rail/compare/v0.2.2...v0.3.0)    | 2025-12-11               | Expanded target discovery, feature exclusions, MSRV analysis, and Cargo argument output.                                                                                                               |
| [`0.2.x`](https://github.com/loadingalias/cargo-rail/compare/v0.1.0...v0.2.2)    | 2025-12-05 to 2025-12-10 | Corrected nested-workspace change detection and completed the first public CI integration.                                                                                                             |
| [`0.1.0`](https://github.com/loadingalias/cargo-rail/releases/tag/v0.1.0)        | 2025-12-05               | First published release with dependency unification, change detection, split/sync, and initial release automation.                                                                                     |
