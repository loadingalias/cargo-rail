# Benchmarking

The native-cache corpus measures release-binary behavior, not an isolated cache function. It interleaves native Cargo,
Cargo incremental, Cargo-Rail delegated, disabled, bypassed, cold, and warm paths with the pinned specialist-cache
lane. Every available lane must preserve command exits, diagnostics, action censuses, runtime output, and the output
bytes that Cargo-Rail owns. A mismatch rejects the complete interleaved group.

## Local workflow

Use the one-group smoke first when checking the harness or host:

```bash
just bench-native-cache-smoke
```

A smoke runs one check and release-build group and validates diagnostic integrity, output equivalence, and zero false
hits.

Choose the retained group count for the decision being made. One group is a correctness check; repeated groups support
p50/p95 and paired comparisons. Run timing measurements on an idle host:

```bash
just bench-native-cache 10
```

Results default to `target/benchmarks/native-cache/<UTC timestamp>/`. Set `CARGO_RAIL_BENCH_RESULTS` to choose an
empty result directory. A run refuses to overwrite a non-empty directory.

Each result has one measurement authority and rebuildable projections:

- `environment.json` binds the repository commit and worktree, untracked content, release binary, benchmark harness,
  toolchain, host, filesystem, and environment.
- `run.json` binds the workload, lanes, sample count, fixture, interleaving policy, Cargo-Rail toolchain identity, and
  specialist version. `native-cache-capability.json` retains the exact identity inspection.
- `raw/<workload>/round-N/group.json` is an atomically finalized interleaved measurement group. Incomplete lane
  directories have no authority. Cargo-Rail cold/warm timing uses the ordinary compact-output command. A separate
  unmeasured replay recreates the identical pre-run target and cache state, adds `--explain`, and retains unit and
  cache-I/O evidence. The group is rejected unless measured and replayed outcomes, action censuses, and outputs agree.
- `samples.jsonl`, `invalid-samples.jsonl`, `compiler-event-diffs.json`, and `summary.json` are derived views.

Set both `CARGO_RAIL_BENCH_L2_ALIAS` and `CARGO_RAIL_BENCH_L2_TARGETS_FILE` to add one same-root operational L2 cycle.
The target must be a machine-owned, read-write S3 authority that permits the fixture's `CARGO_PKG_VERSION` input. The
cycle records one publication, one empty-L1 hit, and one credential-unavailable fallback under `l2/`; it validates the
same compiler action and output bytes across all three commands. Request counts cover authenticated wrapper requests,
and network bytes cover verified result-pack payloads rather than S3 protocol overhead. Retain an exact S3 namespace
inventory separately when the benchmark host has no inventory client.

Rebuild a partial summary at any time, validate a completed run, or resume it on the same machine:

```bash
just bench-native-cache-summarize target/benchmarks/native-cache/<run>
just bench-native-cache-validate target/benchmarks/native-cache/<run>
just bench-native-cache-resume target/benchmarks/native-cache/<run>
```

Resume recreates deterministic fixture and seed state, retains finalized groups, replaces an incomplete group, and
continues the fixed interleaving schedule. It refuses to combine measurements after any bound identity changes. Start
a new result directory after changing source, the harness, binary, toolchain, host, filesystem, or specialist.

`summary.json` reports p50/p95 wall and complete CPU time, complete peak RSS, acquisition and compiler counts, cache
bytes, restored bytes, cache growth, publication repayment, paired comparisons, rejected samples, unsupported lanes,
and false-hit evidence. On Linux, the harness runs sccache's server in the foreground under a separate process-tree CPU
authority and samples aggregate RSS across the command and server process forests every 10 ms. Specialist wall time
adds the isolated server's startup interval to the ordinary command interval. On hosts without this accounting
boundary, raw client counters remain retained but are not projected as complete specialist CPU or RSS results.

Validation requires every requested group, the requested accepted sample count for every available lane, output and
runtime equivalence, and zero false hits. Cargo-Rail cold/warm lanes run for every toolchain whose exact Cargo, rustc,
rustdoc, sysroot, and host identity can be captured. Identity-capture failure is a benchmark failure, not an unsupported
performance lane or a request to add the toolchain to an allowlist.

## Current evidence

The v10 contract invalidates measurements from earlier execution contracts. The retained
`readme-v10-l1-macos-arm64` corpus ran ten interleaved groups on an Apple M1 Pro with macOS 26.5.2, APFS,
and Rust 1.95.0. The release binary came from commit `8a03afb0ba9d2a48cbadbb7c4db41498984290be`; the captured
worktree diff contains only the benchmark ownership correction that runs native-cache capability inspection from the
materialized fixture instead of discovering unrelated policy from the outer repository. The environment record binds
that diff, the harness, both binaries, toolchain, host, fixture, lockfile, and every benchmark control.

Both native Cargo and Cargo-Rail clean-target lanes used disabled incremental compilation, offline dependencies, and an
empty target directory. Cargo-Rail's warm lane used a fresh copy of the populated local CAS:

| Workload and lane | p50 wall | Empirical p95 wall | p50 process-tree CPU | Empirical p95 CPU |
|---|---:|---:|---:|---:|
| Native Cargo `check` | 6.65 s | 7.45 s | 31.70 s | 33.83 s |
| Cargo-Rail warm-L1 `check` | 4.91 s | 6.43 s | 13.44 s | 13.99 s |
| Native Cargo `build --release` | 9.88 s | 10.89 s | 34.59 s | 37.31 s |
| Cargo-Rail warm-L1 `build --release` | 7.40 s | 7.80 s | 17.91 s | 18.73 s |

The paired median wall reduction was 25.7% for `check` and 27.3% for `build`. Exact nonparametric p50 intervals were
19.6%–31.6% and 23.6%–28.7%, respectively, with at least 95% one-sided coverage at both bounds. Lane-median
process-tree CPU fell 57.6% for `check` and 48.2% for `build`. Ten groups do not provide a complete upper confidence
bound for p95, so the table labels p95 as an empirical order statistic rather than a population-tail claim.

All 20 groups and 220 lane samples were accepted. Output manifests, runtime output, diagnostics, action censuses,
compiler-event identities, cache accounting, and measured-versus-proof-replay outcomes matched. The validator found
zero rejected samples and zero false hits. Each Cargo-Rail warm command accepted 26 verified hits, restored the exact
stored results, and bypassed 31 ungraduated invocations.

This comparison measures an empty `target/` with a warm L1. It does not measure Cargo with an intact warm target, where
Cargo-Rail delegates to Cargo's fingerprints, or first-import L2 performance, which includes S3 latency and payload
transfer. The exact p50 confidence interval for the Cargo-Rail-versus-sccache difference includes zero for both
workloads, so this corpus does not support a superiority claim.

One retained Linux x64 operational observation separately covers native Cargo, sccache, L1, L2 publication, empty-L1
import, and unavailable-L2 fallback. Retained macOS ARM64, Linux x64, Windows x64, and Windows ARM64 proofs establish
same-root L2 operation and exact-output preservation; they do not form one timing population.

## Maintainer AWS runners

The maintainer workflow delegates provider mechanics and credentials to `~/dev-machines`; benchmark orchestration adds
no cloud-state or provider abstraction. Benchmark sccache lanes use machine-local storage, and the ordinary comparison
lanes remove S3 and other specialist remote-cache environment variables. The opt-in L2 cycle is the only lane that
uses Cargo-Rail's pinned S3 target authority.

The bounded AWS matrix is:

| Target | Host | Shape | Purpose |
|---|---|---|---|
| `aws-linux-x64-bench` | Ubuntu 24.04 x86-64 | `c8i.xlarge` | Linux x86-64 correctness and performance |
| `aws-linux-arm64-bench` | Ubuntu 24.04 ARM64 | `c8g.xlarge` | Linux ARM64 correctness and performance |
| `aws-windows-x64-bench` | Windows Server 2025 x86-64 | `c8i.xlarge` | Windows x86-64 correctness and performance |

These are fixed-performance compute instances. Burstable and Flex instances are cheaper in some conditions but add
credit or baseline variability to CPU-bound compiler measurements. Windows ARM64 is not in the AWS matrix because
[AWS documents that Windows Server runs only on x86 processors](https://docs.aws.amazon.com/prescriptive-guidance/latest/optimize-costs-microsoft-workloads/right-size-selection.html).
Adding Azure solely to fill that cell would create another provider lifecycle without improving the current cache
experiment.

Always validate the current AMI, permissions, region availability, metadata policy, tags, volume mapping, and launch
request without spending first:

```bash
just bench-native-cache-aws-plan aws-linux-x64-bench
just bench-native-cache-aws-plan aws-linux-arm64-bench
just bench-native-cache-aws-plan aws-windows-x64-bench
```

An executed run requires the explicit `--execute` token. Inspect the dry-run plan and pass smoke before starting a
longer paid run:

```bash
just bench-native-cache-aws-smoke aws-linux-x64-bench --execute
just bench-native-cache-aws aws-linux-x64-bench 10 --execute
```

The wrapper creates one project-scoped instance, synchronizes the primary checkout one way, bootstraps only the
benchmark tools, and runs the corpus. It then atomically freezes only that run, resumes its digest-bound transfer for
up to three attempts, and publishes the verified result under `target/benchmarks/remote/<target>/<UTC run id>/` with
one local rename. Unrelated remote results are never collected, and an interrupted transfer never authorizes a local
result. A sibling `<UTC run id>.orchestration.log` retains the complete create-through-teardown transcript even when
compilation fails before the benchmark can create raw results. The wrapper attempts collection after any benchmark
exit.

The benchmark bootstrap stays lean. Before using a live benchmark host for full validation, install the pinned
check/test tools and run the repository gates explicitly:

```bash
just ssh-qualification-tools aws-linux-x64-bench
just ssh-just aws-linux-x64-bench check
just ssh-just aws-linux-x64-bench build-all
just ssh-just aws-linux-x64-bench test-all
```

Use the matching Windows target for Windows validation. This profile installs `cargo-nextest`, `cargo-deny`, and
`cargo-audit`; it does not add those tools to ordinary benchmark-only images.

The exit trap then invokes `dev-machine kill`. AWS instances use IMDSv2-only metadata, encrypted gp3 root volumes,
guest-shutdown termination, and `DeleteOnTermination=true`. Teardown waits for instance termination, finds every EBS
volume with the Cargo-Rail project and target tags, deletes residual volumes, and fails if any remain. The guest also
terminates after five idle minutes if the local orchestration process disappears.

If teardown reports a failure, retry and inspect the exact target before doing anything else:

```bash
just ssh-kill aws-linux-x64-bench
just ssh-status aws-linux-x64-bench
```

Do not compare absolute timings across hosts as if they were one population. Each host produces a separate corpus
bound to its own CPU, operating system, filesystem, toolchain, and target. Choose enough same-host groups for the claim:
one for a correctness smoke test, repeated groups for distribution or tail comparisons.
