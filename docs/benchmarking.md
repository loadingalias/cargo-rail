# Benchmarking

## Planner qualification

`just bench-plan-smoke` builds `cargo-rail` from the current source in an isolated target directory, then runs one
sample in each planner lane. The retained `results.json` binds HEAD, the index and non-ignored worktree, the candidate
and harness digests, the toolchain, the host, every raw sample, and the consumer-reader measurement. A smoke result has
`status = "completed_unqualified"`; it checks the harness but does not qualify latency.

Run the 20-sample qualification only on a quiet pinned machine:

```bash
just bench-plan 20
```

Results default to `target/benchmarks/plan/<UTC timestamp>/results.json`. The harness writes
`status = "incomplete"` before it builds or measures anything. It persists each accepted sample before checking the
budgets, then seals the result as `qualified`, `completed_unqualified`, or `failed`. A failed result retains
`failure_reason`; do not delete it before diagnosing a missed bound.

An external candidate is a separate comparison mode. It requires a provenance sidecar and never represents the
current checkout implicitly:

```json
{
  "schema_version": 1,
  "source_identity": "sha256:<64 lowercase hexadecimal digits>",
  "binary_sha256": "<64 lowercase hexadecimal digits>"
}
```

```bash
python3 scripts/bench/plan.py smoke \
  --candidate /path/to/cargo-rail \
  --candidate-provenance /path/to/provenance.json
```

The harness verifies the binary digest before creating fixtures or collecting samples. Keep any additional provenance
fields in the sidecar; the result retains the sidecar path and digest with the declared immutable source identity.

## Source-surface analysis

Compare `cargo rail surface` and Hawk with the same source commit, compiler identity, production products, feature
profiles, target, doctest packages, lint policy, output sink, and cache state. Compare cold with cold and repeated with
repeated. A Cargo-Rail fact hit against a cold Hawk target is not an implementation-speed result.

The acquisition model predicts subprocess work before measurement. For one feature/target view, let `P` be the number
of configured production products and `D` the number of selected doctest package passes:

| Tool               | Ordinary target passes | Doctest passes | Total Cargo acquisition views |
| ------------------ | ---------------------: | -------------: | ----------------------------: |
| Cargo-Rail         |                      1 |            `D` |                       `1 + D` |
| Reference analyzer |                `P + 1` |            `D` |                   `P + 1 + D` |

Cargo-Rail classifies production and non-production units from one all-targets compiler acquisition. It uses fewer
Cargo acquisition views when `P > 0`, but this does not guarantee a wall-time ratio. Dependency freshness, linking,
doctests, cache eligibility, scheduling, and output volume can dominate.

`tests/surface-reference.json` is the qualification authority. It binds Hawk 0.1.13 by commit and archive digest, Rust
1.98.0, the host target, two shipped products, all features, doctest packages, lint policy, focused cases, and exact
difference allowlists. Run it against an extracted native Cargo-Rail archive so the measurement includes the adjacent
compiler-fact driver:

```bash
scripts/ci/qualify-surface-reference.py \
  --cargo-rail /path/to/extracted/cargo-rail \
  --reference-archive /path/to/hawk-a3b75f193b931d11cf8883c44bda3f9a79c8f19a.tar.gz \
  --output benchmark_results/surface-reference-$(date +%Y%m%d) \
  --runs 20
```

The harness performs a separate conformance smoke and then interleaves 20 accepted samples in each lane:

| Lane             | Target and cache authority                                                                                       |
| ---------------- | ---------------------------------------------------------------------------------------------------------------- |
| `cold-target`    | Fresh reference target, fresh Cargo-Rail analysis target, and fresh fact cache                                   |
| `cargo-fresh`    | Reused reference Cargo target; Cargo-Rail conservatively bypasses facts because the fixture gains a build script |
| `fact-cache-hit` | Repeated unchanged workspace with a required complete Cargo-Rail fact hit                                        |
| `cache-bypass`   | Fresh target with the build-script observation boundary forcing a named fact-cache bypass                        |

Every accepted pair must retain equivalent normalized findings after exact allowlist application. Each raw sample
keeps stdout, stderr, the timer record, normalized findings, exact argv, wall and CPU time, peak RSS, Cargo view count,
compiler invocation count, fact-cache outcome, and exit code. `identity.json` binds the source commit, worktree patch,
fixture tree, tool and driver bytes, compiler, Cargo, host, target, profiles, doctests, and output sink. `summary.json`
reports p50/p95 without deleting a slow, noisy, or failed corpus.

A “2x faster” claim requires equivalent cold Cargo-Rail p50 and p95 at or below 50% of Hawk on the retained corpus. If
the bound fails, report the measured result without the speed claim.

The native-cache workflow measures transparent local activation under direct Cargo. It excludes retired runner-owned
lanes and remote activation. This establishes the L0/L1 baseline, not S3 correctness or performance.

## Native cache lanes

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

Each lane has an isolated Cargo home and cache authority below the result directory. Setup uses
`cargo rail cache setup`; the developer's Cargo configuration is not installation authority. Supported platforms may
link registry and Git dependency stores read-only from the invoking Cargo home. Configuration, receipts, wrappers,
sessions, ledgers, CAS, target directories, and sccache data remain isolated.

## Local qualification

```bash
just bench-native-cache-smoke
```

One accepted sample per lane checks orchestration, output bytes, expected cache outcomes, zero false hits, and
reporting. It does not qualify performance.

Run the canonical interleaved qualification corpus next:

```bash
just bench-native-cache 20
```

- Cargo L0 and `CARGO_RAIL_CACHE=off` p95 overhead is at most the larger of 3% or 50 ms;
- transparent empty-target L1 is at least 10% faster at both p50 and p95 than accepted `sccache-server` (the target
  is 15%); and
- every accepted sample has exact outputs, an explicit cache outcome, and zero ambiguity or false hits.

Twenty accepted samples per lane form one qualification corpus. A reviewed anomaly may use at most 30. Retain a
failed corpus, profile its failing lane if justified, and make no superiority claim. Do not increase the sample count
to average away a missed bound.

## Remote provider qualification

Remote correctness uses a disposable provider authority and independent roots with empty local caches. AWS S3, Azure
Blob Storage, and Cloudflare R2 must each prove producer/consumer import, conditional publication, read-only operation,
a subsequent L1 hit without a remote request, exact outputs, and cold fallback for absence, corruption, and outage.
Retained evidence excludes credentials. Cleanup verifies the disposable prefix, multipart uploads, and provider
resources are absent.

The direct-provider harness retains a historical S3 name but accepts normalized AWS S3, Azure Blob, and R2 URLs. One
fault harness preserves each provider's CLI, endpoint, and credential authority. Cleanup remains provider-specific:

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

### S3 performance

Remote performance qualification uses a disposable real AWS S3 authority and independent machines with empty local
caches.

A local harness, loopback test, or SDK mock is not remote performance evidence. Interleave empty-L1 S3 import with the
pinned remote sccache lane and retain 20–30 samples per workload and lane. Twenty rounds produce 120 samples across
three workloads and two lanes; use at most 30 rounds for a reviewed anomaly. The target is 15% faster at p50 and p95.
At least 10% at both percentiles is an accepted win; below 10% proves availability, not superiority. Claims also
require zero false hits, strict containment of sccache's safe Rust hits, and reported bytes, requests, CPU, memory,
eligibility, conflicts, and operational requirements.

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

## Platform qualification

Timing does not prove compiler-cache correctness. The integration fixture separately proves exact `.d`,
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

## Claim requirements

Pre-linked and runner-owned L2 measurements describe different architectures and do not support this contract. A
local shared-hit superiority claim requires one accepted 20-sample corpus with `superiority_qualified: true`;
`target_qualified` records the 15% target. A broader local performance claim also requires
`performance_qualified: true`, including the L0 and cache-off overhead bounds. A remote claim requires the separate
real-backend corpus. Retain a failed corpus for its exact worktree and host. Do not replace it with a larger population
or combine it with another run.

Do not compare absolute timings across hosts or combine distinct commits, worktrees, toolchains, target-state policies,
physical roots, cache protocols, or machines into one population.
