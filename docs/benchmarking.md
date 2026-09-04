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

## Claim requirements

Publish a performance claim only when:

- the retained result reports the claimed qualification as passed;
- correctness, output identity, operation coverage, and cache outcomes are complete;
- the workload, host, toolchain, commands, sample count, and before/after values are stated; and
- the claim is limited to the measured architecture, platform, and workload.

A failed corpus is useful evidence. Report the measured result without the failed speed or superiority claim.
