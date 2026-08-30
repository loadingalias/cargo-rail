# Benchmarking

A performance claim needs retained correctness evidence from the exact measured source, binary, toolchain, host, and
workload. A smoke run validates orchestration; it does not qualify latency.

## Measurement rules

1. Compare one change at a time on the same quiet machine.
2. Bind the commit, non-ignored worktree, binary, harness, toolchain, host, arguments, and cache state.
3. Reject a sample before timing analysis when outputs, coverage, or expected cache outcomes differ.
4. Retain raw samples and failed corpora; do not increase the population to hide a missed bound.
5. Report one sample as a point measurement. Use p50 or p95 only for a retained multi-sample corpus.

Do not combine different commits, worktrees, toolchains, target-state policies, physical roots, cache protocols, or
machines into one population.

## Planner

Validate the harness first:

```bash
just bench-plan-smoke
```

Run the qualification corpus only on a quiet pinned machine:

```bash
just bench-plan 20
```

Results live under `target/benchmarks/plan/`. `results.json` records raw samples, source and binary identity, host,
toolchain, harness identity, and the final `qualified`, `completed_unqualified`, or `failed` status. Keep
`failure_reason` and the accepted samples when a bound fails.

An external candidate requires a provenance sidecar that binds its binary digest to an immutable source identity. The
harness verifies that digest before creating fixtures or collecting samples.

## Surface

Compare Surface and a reference analyzer with the same source, compiler, products, features, target views, doctests,
lint policy, output sink, and cache state. Compare cold with cold and repeated with repeated.

`tests/surface-reference.json` owns the pinned reference, compiler, fixture, and accepted differences. Run the
qualification harness against an extracted native Cargo-Rail archive so the compiler driver is part of the measured
installation:

```bash
scripts/ci/qualify-surface-reference.py \
  --cargo-rail /path/to/extracted/cargo-rail \
  --reference-archive /path/to/reference.tar.gz \
  --output benchmark_results/surface-reference \
  --runs 20
```

Accept timing only after normalized findings match under the checked-in allowlist. Fewer Cargo acquisition views do
not prove a wall-time ratio.

## Native compiler cache

Run one sample per lane to validate orchestration and exact outputs:

```bash
just bench-native-cache-smoke
```

Run the retained interleaved qualification corpus:

```bash
just bench-native-cache 20
```

The harness separates Cargo L0, installed-wrapper L0, native cold, cache disabled, Cargo-Rail cold and warm, pinned
sccache, and unsupported incremental lanes. Each lane has isolated Cargo, target, receipt, and cache state. The
result's checked thresholds—not prose in this guide—decide whether overhead, correctness, coverage, and comparative
performance qualify.

Results live under `target/benchmarks/native-cache/`. Preserve `environment.json`, `run.json`, coverage inventories,
raw lane output, `samples.jsonl`, and `summary.json`. Rebuild, validate, or resume projections without changing the
physical state root:

```bash
just bench-native-cache-summarize target/benchmarks/native-cache/<run>
just bench-native-cache-validate target/benchmarks/native-cache/<run>
just bench-native-cache-resume target/benchmarks/native-cache/<run>
```

Resume refuses a changed repository or worktree. Start a new result after changing the source, harness, binary,
toolchain, host, cache policy, or protocol.

Root-portability evidence must cover same-root and independent-root checkouts, an external `CARGO_TARGET_DIR`, and a
same-size mutation of a compiler-selected repository input that produces a miss. Report full source-capture work
separately from the bounded selected-input refresh; a warm lookup is not credible if it hides either cost.

## Remote and platform qualification

A loopback fixture or SDK mock proves protocol behavior, not provider performance. Cross-root remote evidence needs a
producer and read-only consumer with empty local caches, a passing pair report, exact producer keys for every imported
action, byte-identical consumer outputs after offline L1 replay, and explicit absence, corruption, and outage fallback.
Broad compiler-class coverage may retain unsupported actions as safe read-only misses; do not misrepresent those
root-bound outputs as portable. Use the `qualify-native-cache-*`, `validate-native-cache-remote-pair`, and
provider-specific cleanup recipes listed by `just --list`.

Remote performance needs disposable real provider authority and independent machines. Interleave Cargo-Rail import
with the pinned remote comparator; retain provider requests, bytes, cache outcomes, selected-input mutation and
external-target-root evidence, resource use, and cleanup proof.

Timing does not prove platform correctness. Run the native-host qualification front doors for every claimed host:

```bash
just qualify-native-cache-native-plan <target>
just qualify-native-cache-native-smoke <target> --execute
just qualify-native-cache-native <target> 20 --execute
```

Distributed execution uses its separate `qualify-distributed-execution-*` workflow. Keep worker capability, sandbox
and resource evidence, local and remote samples, placement decisions, fallbacks, and exact outputs in one retained
run.

## Claim requirements

Publish a performance claim only when:

- the retained result reports the claimed qualification as passed;
- correctness, output identity, operation coverage, and cache outcomes are complete;
- the workload, host, toolchain, commands, sample count, and before/after values are stated; and
- the claim is limited to the measured architecture, platform, and workload.

A failed corpus is useful evidence. Report the measured result without the failed speed or superiority claim.
