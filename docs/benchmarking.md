# Benchmarking

The native-cache corpus measures release-binary behavior, not an isolated cache function. It interleaves native Cargo,
Cargo incremental, Cargo-Rail delegated, disabled, bypassed, cold, and warm paths with the pinned specialist-cache
lane. Every available lane must preserve command exits, diagnostics, action censuses, runtime output, and the output
bytes that Cargo-Rail owns. A mismatch rejects the complete interleaved group.

## Local workflow

The default retained run measures one accepted sample per available lane, without a warmup or separate unit-timing
pass:

```bash
just bench-native-cache
```

Use a smoke run when validating only the harness or its environment:

```bash
just bench-native-cache-smoke
```

A smoke also runs one check and release-build sample per lane, but sets `publishable` to `false`. Increase the retained
sample count only when the decision needs a timing distribution or variance estimate:

```bash
just bench-native-cache 10
```

Results default to `target/benchmarks/native-cache/<UTC timestamp>/`. Set `CARGO_RAIL_BENCH_RESULTS` to choose an
empty result directory. A run refuses to overwrite a non-empty directory.

Each result has one measurement authority and rebuildable projections:

- `environment.json` binds the repository commit and worktree, untracked content, release binary, benchmark harness,
  toolchain, host, filesystem, and environment.
- `run.json` binds the workload, lanes, sample budget, fixture, interleaving policy, Cargo-Rail toolchain identity, and
  specialist version. `native-cache-capability.json` retains the exact identity inspection.
- `raw/<workload>/round-N/group.json` is an atomically finalized interleaved measurement group. Incomplete lane
  directories have no authority. Cargo-Rail cold/warm timing uses the ordinary compact-output command. A separate
  unmeasured replay recreates the identical pre-run target and cache state, adds `--explain`, and retains unit and
  cache-I/O evidence. The group is rejected unless measured and replayed outcomes, action censuses, and outputs agree.
- `samples.jsonl`, `invalid-samples.jsonl`, `compiler-event-diffs.json`, and `summary.json` are derived views.

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
bytes, restored bytes, cache growth, publication repayment, budget results, rejected samples, unsupported lanes, and
false-hit evidence. On Linux, the harness runs sccache's server in the foreground under a separate process-tree CPU
authority and samples aggregate RSS across the command and server process forests every 10 ms. Specialist wall time
adds the isolated server's startup interval to the ordinary command interval. On hosts without this accounting
boundary, raw client counters remain retained but are not projected as complete specialist CPU or RSS results.

A retained result is publishable only when every planned group is finalized, every available lane reaches its sample
budget, and false hits remain zero. Cargo-Rail cold/warm lanes run for every toolchain whose exact Cargo, rustc,
rustdoc, sysroot, and host identity can be captured. Identity-capture failure is a benchmark failure, not an
unsupported performance lane or a request to add the toolchain to an allowlist.

## Maintainer AWS runners

The maintainer workflow delegates provider mechanics and credentials to `~/dev-machines`. Cargo-Rail adds no AWS SDK,
cloud state, remote cache, or provider abstraction. Benchmark sccache lanes use machine-local storage; the harness
removes S3 and other remote-cache environment variables before measurement.

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

An executed smoke or retained run requires the explicit `--execute` token. Both default to one sample per lane:

```bash
just bench-native-cache-aws-smoke aws-linux-x64-bench --execute
just bench-native-cache-aws aws-linux-x64-bench --execute
```

The wrapper creates one project-scoped instance, synchronizes the primary checkout one way, bootstraps only the
benchmark tools, and runs the corpus. It then atomically freezes only that run, resumes its digest-bound transfer for
up to three attempts, and publishes the verified result under `target/benchmarks/remote/<target>/<UTC run id>/` with
one local rename. Unrelated remote results are never collected, and an interrupted transfer never authorizes a local
result. A sibling `<UTC run id>.orchestration.log` retains the complete create-through-teardown transcript even when
compilation fails before the benchmark can create raw results. The wrapper attempts collection after any benchmark
exit.

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
bound to its own CPU, operating system, filesystem, toolchain, and target. One complete same-host group supports a
bounded engineering comparison. Repeat the group only when the decision requires tail or variance claims.
