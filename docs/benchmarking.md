# Benchmarking

The native-cache workflow measures transparent local activation under direct Cargo. Runner-owned cache lanes are
retired because they are not part of the compiler-cache architecture. The harness deliberately leaves remote cache
activation disabled: it establishes the L0/L1 baseline, not official S3 correctness or performance.

## Questions measured separately

The real-world fixture runs `cargo check` and `cargo build --release` in deterministic interleaved lane order:

| Lane | Target state | Cache state | Question |
|---|---|---|---|
| `cargo-l0` | intact | no wrapper | Native Cargo no-op authority |
| `transparent-l0` | intact | installed L1 | Transparent no-op overhead |
| `cargo-empty` | empty | no wrapper | Native cold-target baseline |
| `cache-off` | empty | installed but `CARGO_RAIL_CACHE=off` | Exact opt-out overhead |
| `transparent-cold` | empty | empty receipt-selected L1 | Cold publication overhead |
| `transparent-warm` | empty | verified same-root L1 | Restored compiler work |
| `sccache-server` | empty | warm pinned local sccache | Specialist server baseline |
| `sccache-client` | empty | warm pinned client-side sccache | Specialist daemonless baseline |
| `transparent-incremental` | empty | installed L1, incremental forced | Unsupported early bypass |

Each lane has an isolated Cargo home and cache authority below the result directory. Setup is performed through
`cargo rail cache setup`; the developer's Cargo configuration is never read or changed as installation authority.
Registry and Git dependency stores may be linked read-only from the invoking Cargo home on platforms that support
links, but configuration, receipt, wrapper, sessions, ledgers, CAS, target directories, and sccache data remain
isolated.

## Run the five-sample local qualification corpus

```bash
just bench-native-cache-smoke
```

One accepted sample per lane checks orchestration, exact lane-local authoritative output bytes, expected cache outcomes,
zero false hits, and reporting. It is not performance qualification.

Run the canonical interleaved qualification corpus next:

```bash
just bench-native-cache 5
```

- Cargo L0 and `CARGO_RAIL_CACHE=off` p95 overhead is at most the larger of 3% or 50 ms;
- transparent empty-target L1 is strictly faster at both p50 and p95 than accepted `sccache-server`; and
- every accepted sample has exact outputs, an explicit cache outcome, and zero ambiguity or false hits.

Five accepted samples per lane are the complete qualification unit. A failed corpus is useful evidence: retain it,
profile the failing lane when further optimization is justified, and make no superiority claim. Do not expand the
sample count merely to average away a missed bound.

## Qualify remote providers separately

Remote correctness qualification uses a disposable real provider authority and independent roots with empty local
caches. AWS S3, Azure Blob Storage, and Cloudflare R2 must each prove producer/consumer import, conditional
publication, read-only operation, a subsequent valid L1 hit with no remote request, exact outputs, and cold fallback
for absence, corruption, and outage. Provider credentials stay outside retained evidence, and cleanup must verify the
exact disposable prefix, multipart uploads, and provider resources are absent.

The retained direct-provider harness has a historical S3 name but accepts normalized AWS S3, Azure Blob, and R2 URLs.
R2 adds guarded fault and cleanup recipes because its S3-compatible transport still has distinct endpoint and
credential authority:

```bash
just qualify-native-cache-s3 <producer|consumer> <run-id> <remote-url>
just qualify-native-cache-r2-faults <run-id> <r2-url> <bucket>
just cleanup-native-cache-r2-prefix <account-id> <bucket> <exact-task-prefix>
```

The R2 fault recipe can resume an interrupted outage assertion without repeating the destructive corruption step by
using `qualify-native-cache-r2-faults-resume-outage` with the same arguments.

### Qualify official S3 performance separately

Remote performance qualification uses a disposable real AWS S3 authority and independent machines with empty local
caches.

Do not treat this local harness, a loopback protocol test, or an SDK mock as remote performance evidence. A remote
comparison must interleave empty-L1 S3 import with the accepted pinned remote sccache lane and retain 20–30 accepted
samples across the complete matrix. Five to seven rounds produce 20–28 total samples, with 5–7 samples in each of the
two workloads and two lanes; the harness refuses a larger population. The target is at least 15% faster at both p50
and p95. At least 10% at both percentiles is an accepted win; below 10% is an availability result, not superiority.
Every accepted claim also requires zero false hits and reported bytes, requests, CPU, memory, eligibility, conflicts,
and operational requirements.

## Evidence layout

Results default to `target/benchmarks/native-cache/<UTC timestamp>/`. Set `CARGO_RAIL_BENCH_RESULTS` to choose an empty
directory.

- `environment.json` binds the commit, complete non-ignored worktree patch, release binary, harness, Cargo, rustc,
  pinned sccache, host, and local-only authority. `worktree.diff` includes untracked file bytes so a retained result can
  reconstruct the measured source state; keep credentials and other private data in ignored files.
- `run.json` binds workloads, lanes, sample count, rotation, and qualification thresholds.
- `coverage/<workload>/coverage.json` is the authoritative per-action coverage and cold-cost join. It records the Cargo
  census, Cargo-Rail and sccache outcomes, matched/extra/missing actions, ambiguity, logical output roles/bytes, and one
  complete serial cold timing record per rustc action. Timing claims are invalid unless this ledger is complete, has
  zero ambiguity, and shows that Cargo-Rail's verified hit set strictly exceeds sccache's accepted hit set.
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
  overheads, sccache reductions, false hits, and whether the complete five-sample qualification passed.

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
`.rmeta`, `.rlib`, build-script executable, proc-macro producer, and final linked bytes; diagnostic replay; root
isolation; environment and linker-input invalidation; L0 no-op behavior; L1 outage fallback; compiler-class
boundaries; and same-root cold-then-warm reuse. Platform qualification must run those oracles on every claimed host
even when no performance claim is made.

## Qualification claims

Earlier pre-linked and runner-owned L2 measurements describe different architectures and are not evidence for this
contract. Publish a scoped local performance claim only from one accepted five-sample corpus whose summary reports
`performance_qualified: true`. Publish a scoped remote claim only from the separate real-backend corpus above. Retain
a failed corpus as the canonical result for that exact worktree and host; do not replace it with a larger population
or combine it with another run.

Do not compare absolute timings across hosts or combine distinct commits, worktrees, toolchains, target-state policies,
physical roots, cache protocols, or machines into one population.
