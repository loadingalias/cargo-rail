# Benchmarking

The native-cache workflow measures transparent activation under direct Cargo. Runner-owned cache lanes and operational
L2 lanes are retired because neither is part of the compiler-cache architecture.

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

## Run the five-sample qualification corpus

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

## Evidence layout

Results default to `target/benchmarks/native-cache/<UTC timestamp>/`. Set `CARGO_RAIL_BENCH_RESULTS` to choose an empty
directory.

- `environment.json` binds the commit, worktree diff, release binary, harness, Cargo, rustc, pinned sccache, host, and
  local-only authority.
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

Earlier pre-linked and operational L2 measurements describe different architectures and are not evidence for this
contract. Publish a scoped performance claim only from one accepted five-sample corpus whose summary reports
`performance_qualified: true`. Retain a failed corpus as the canonical result for that exact worktree and host; do not
replace it with a larger population or combine it with another run.

Do not compare absolute timings across hosts or combine distinct commits, worktrees, toolchains, target-state policies,
physical roots, cache protocols, or machines into one population.
