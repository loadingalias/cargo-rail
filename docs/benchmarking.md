# Benchmarking

The native-cache workflow measures transparent local activation under direct Cargo. Runner-owned cache lanes are
retired because they are not part of the compiler-cache architecture. The harness deliberately leaves remote cache
activation disabled: it establishes the L0/L1 baseline, not official S3 correctness or performance.

## Questions measured separately

The real-world fixture runs `cargo check`, `cargo build --release`, and `cargo test --no-run --all-targets` in
deterministic interleaved lane order:

| Lane                      | Target state | Cache state                          | Question                       |
| ------------------------- | ------------ | ------------------------------------ | ------------------------------ |
| `cargo-l0`                | intact       | no wrapper                           | Native Cargo no-op authority   |
| `transparent-l0`          | intact       | installed L1                         | Transparent no-op overhead     |
| `cargo-empty`             | empty        | no wrapper                           | Native cold-target baseline    |
| `cache-off`               | empty        | installed but `CARGO_RAIL_CACHE=off` | Exact opt-out overhead         |
| `transparent-cold`        | empty        | empty receipt-selected L1            | Cold publication overhead      |
| `transparent-warm`        | empty        | verified same-root L1                | Restored compiler work         |
| `sccache-server`          | empty        | warm pinned local sccache            | Specialist server baseline     |
| `sccache-client`          | empty        | warm pinned client-side sccache      | Specialist daemonless baseline |
| `transparent-incremental` | empty        | installed L1, incremental forced     | Unsupported early bypass       |

Each lane has an isolated Cargo home and cache authority below the result directory. Setup is performed through
`cargo rail cache setup`; the developer's Cargo configuration is never read or changed as installation authority.
Registry and Git dependency stores may be linked read-only from the invoking Cargo home on platforms that support
links, but configuration, receipt, wrapper, sessions, ledgers, CAS, target directories, and sccache data remain
isolated.

## Run the 20-sample local qualification corpus

```bash
just bench-native-cache-smoke
```

One accepted sample per lane checks orchestration, exact lane-local authoritative output bytes, expected cache outcomes,
zero false hits, and reporting. It is not performance qualification.

Run the canonical interleaved qualification corpus next:

```bash
just bench-native-cache 20
```

- Cargo L0 and `CARGO_RAIL_CACHE=off` p95 overhead is at most the larger of 3% or 50 ms;
- transparent empty-target L1 is at least 10% faster at both p50 and p95 than accepted `sccache-server` (the target
  is 15%); and
- every accepted sample has exact outputs, an explicit cache outcome, and zero ambiguity or false hits.

Twenty accepted samples per lane are the canonical qualification unit. A reviewed anomaly may use at most 30, but a
failed corpus remains useful evidence: retain it, profile the failing lane when further optimization is justified, and
make no superiority claim. Do not expand the sample count merely to average away a missed bound.

## Qualify remote providers separately

Remote correctness qualification uses a disposable real provider authority and independent roots with empty local
caches. AWS S3, Azure Blob Storage, and Cloudflare R2 must each prove producer/consumer import, conditional
publication, read-only operation, a subsequent valid L1 hit with no remote request, exact outputs, and cold fallback
for absence, corruption, and outage. Provider credentials stay outside retained evidence, and cleanup must verify the
exact disposable prefix, multipart uploads, and provider resources are absent.

The retained direct-provider harness has a historical S3 name but accepts normalized AWS S3, Azure Blob, and R2 URLs.
One guarded fault harness preserves each provider's CLI, endpoint, and credential authority; cleanup remains
provider-specific:

```bash
just qualify-native-cache-s3 <producer|consumer> <run-id> <remote-url>
just validate-native-cache-remote-pair \
  benchmark_results/native-cache/<run-id>-producer \
  benchmark_results/native-cache/<run-id>-consumer \
  benchmark_results/native-cache/<run-id>-pair.json
just qualify-native-cache-remote-faults <run-id> <remote-url>
just cleanup-native-cache-s3-prefix <bucket> <region> <exact-task-prefix>
just cleanup-native-cache-azure-container <account> <cargo-rail-task9-run-id> <run-id>
just cleanup-native-cache-r2-prefix <account-id> <bucket> <exact-task-prefix>
```

The producer and consumer phases each exercise check, release build, and all-target test compilation. The pair
validator requires identical repository, worktree, binary, harness, toolchain, provider, and remote authority evidence;
an exact root-independent published/imported action multiset for every workload; byte-identical authoritative outputs;
complete compiler-class coverage; a read-only consumer; and a subsequent L1 hit with no remote work. A passing phase
without a passing pair report is not cross-root evidence.

The producer, consumer, and fault recipes accept normalized AWS S3, Azure Blob, and R2 authorities and require the
exact disposable `cache/cargo-rail/<provider>/v3/task9/<run-id>` prefix. The fault run corrupts one verified entry,
proves exact-output cold fallback, deletes that entry, proves absence fallback, then repeats the cold-output assertion
under a network-denied outage. None of the three fault phases may write remotely. The outage phase requires macOS
`sandbox-exec` so the network boundary is observed rather than inferred from an invalid endpoint. Resume an
interrupted outage assertion without repeating the destructive corruption and absence steps with
`qualify-native-cache-remote-faults-resume-outage`. Azure runs additionally require the dedicated
`cargo-rail-task9-<run-id>` container; cleanup deletes and verifies that entire container. S3 cleanup accepts only the
exact Task 9 Cargo-Rail or comparison-sccache namespace, and R2 cleanup accepts only the exact Task 9 Cargo-Rail
namespace.

### Qualify official S3 performance separately

Remote performance qualification uses a disposable real AWS S3 authority and independent machines with empty local
caches.

Do not treat this local harness, a loopback protocol test, or an SDK mock as remote performance evidence. A remote
comparison must interleave empty-L1 S3 import with the accepted pinned remote sccache lane and retain 20–30 accepted
samples for each workload and lane. Twenty rounds produce 120 accepted samples across the three workloads and two
lanes; the harness permits at most 30 rounds only for a reviewed anomaly. The target is at least 15% faster at both
p50 and p95. At least 10% at both percentiles is an accepted win; below 10% is an availability result, not superiority.
Every accepted claim also requires zero false hits, strict containment of sccache's safe Rust hits, and reported
bytes, requests, CPU, memory, eligibility, conflicts, and operational requirements.

## Evidence layout

Results default to `target/benchmarks/native-cache/<UTC timestamp>/`. Set `CARGO_RAIL_BENCH_RESULTS` to choose an empty
directory.

- `environment.json` binds the commit, complete non-ignored worktree patch, release binary, harness, Cargo, rustc,
  pinned sccache, host, and local-only authority. `worktree.diff` includes untracked file bytes so a retained result can
  reconstruct the measured source state; keep credentials and other private data in ignored files.
- `run.json` binds workloads, lanes, sample count, rotation, and qualification thresholds.
- `coverage/<workload>/coverage.json` is the authoritative operation inventory, cache comparison, and cold-cost join.
  Cargo-Rail emits each root-independent compiler-operation shape and identity; path-bearing and opaque compiler
  values retain typed shapes while the adjacent raw argv preserves forensic detail. The reporter rejects disagreement
  before correlating Cargo and sccache evidence. The fixture compiles `asm!`, `global_asm!`, `naked_asm!`, included
  integrated assembly, target-feature-sensitive assembly, plain and preprocessed external assembly, and a native
  archive. The inventory records Rust compiler, build-script execution, native compilation, external assembly,
  archive, and tool-probe nodes; exact artifact edges; specific cold capabilities; output roles/bytes; cache outcomes;
  and one complete serial cold timing record per rustc action. An
  sccache-only hit is accepted only when the graph proves Cargo-Rail rejected a proc-macro consumer at its explicit
  unobserved dynamic-dependency boundary. Timing claims are invalid unless every required operation class is present,
  every child is terminally accounted for, all correlations are unambiguous, and Cargo-Rail's verified hit set
  strictly exceeds sccache's accepted hit set.
- `coverage/compiler-modes/coverage.json` proves that Clippy diagnostics, rustdoc output trees, and doctest execution
  remain explicit acquisition-free cold operations. The ledger rejects an action/result key, local-cache read, or
  remote request for any of those modes.
- `state/` contains the isolated resumable fixtures, Cargo homes, receipts, target trees, and cache roots. Never treat
  it as a portable result; action identity is physical-root-bound.
- `raw/<workload>/round-N/<lane>/` retains stdout, stderr, timing/resource evidence, cache status deltas, and the
  authoritative output manifest. Every lane hashes modes and bytes for cacheable dep-info and Rust library metadata.
  Private rustc incremental-session files are excluded because their session paths are not Cargo-facing outputs.
  The unsupported incremental control also excludes `.rlib` archives: native incremental codegen rewrites their
  session-dependent bytes across clean-target invocations, and this lane proves early bypass rather than archive reuse.
  The verified warm L1 lane additionally hashes every file Cargo reports as a compiler artifact, including build-script
  executables, proc-macro dylibs, and final binaries. Competitor lanes do not claim exact hits for linked outputs they
  always rebuild, so the harness does not misclassify their linker nondeterminism as a cache-integrity failure.
- `samples.jsonl` and `summary.json` are rebuildable projections. Summary reports sample counts, p50/p95/mean/min/max,
  CPU and peak-RSS evidence, overheads, sccache reductions, false hits, the required/observed/missing compiler classes
  across check/build/test, and whether the complete 20-sample qualification passed.

Rebuild or validate projections, or resume without changing the physical state root:

```bash
just bench-native-cache-summarize target/benchmarks/native-cache/<run>
just bench-native-cache-validate target/benchmarks/native-cache/<run>
just bench-native-cache-resume target/benchmarks/native-cache/<run>
```

Resume refuses a changed repository commit or worktree diff. Start a new result after changing source, harness,
binary, toolchain, host, policy, or cache protocol.

## Correctness is independent

Timing acceptance does not prove compiler-cache correctness. The integration fixture separately proves exact `.d`,
`.rmeta`, `.rlib`, compiler-owned static archives, Rust and C dynamic libraries, build-script executables, proc-macro
producers, and linked binary, test, example, and benchmark bytes; diagnostic replay; root isolation; environment and
linker-input invalidation; L0 no-op behavior; L1 outage fallback; compiler-class boundaries; and same-root
cold-then-warm reuse. Platform qualification must run those oracles on every claimed host even when no performance
claim is made.

The disposable native-host orchestrator accepts `aws-linux-x64-bench`, `aws-linux-arm64-bench`,
`aws-windows-x64-bench`, and `azure-windows-arm64-profile`. It verifies the exact rustc host and filesystem, runs the
front-door compatibility corpus, the authenticated full build/test suite, and doctests, then runs the native-cache
smoke or 20–30-round performance contract. The retained `platform/qualification.json` is true only for a full run whose
correctness, benchmark integrity, and aggregate performance gates all pass.

```bash
just qualify-native-cache-native-plan <target>
just qualify-native-cache-native-smoke <target> --execute
just qualify-native-cache-native <target> 20 --execute
```

## Qualification claims

Earlier pre-linked and runner-owned L2 measurements describe different architectures and are not evidence for this
contract. Publish a scoped local shared-hit superiority claim only from one accepted 20-sample corpus whose summary
reports `superiority_qualified: true`; `target_qualified` records the 15% target. The broader local performance claim
also requires `performance_qualified: true`, which includes the L0 and cache-off overhead bounds. Publish a scoped
remote claim only from the separate real-backend corpus above. Retain a failed corpus as the canonical result for that
exact worktree and host; do not replace it with a larger population or combine it with another run.

Do not compare absolute timings across hosts or combine distinct commits, worktrees, toolchains, target-state policies,
physical roots, cache protocols, or machines into one population.
