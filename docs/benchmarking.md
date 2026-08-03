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

## Retained v4 evidence

The retained corpora used Rust 1.97.1 and one AWS `c8i.xlarge` per host:

- Linux: `20260803T045358Z-aws-linux-x64-bench-v4-final-one`, Intel Xeon 6975P-C, Ubuntu 24.04.
- Windows: `20260803T142934Z-aws-windows-x64-bench-v4-final-one`, Windows Server 2025.

Each row is one accepted point measurement, not a percentile. Each host accepted all 20 planned lane samples in two
complete groups with no rejected samples or false hits. Do not pool the hosts into one population.

| Host | Workload | Native Cargo cold | Cargo-Rail cold | Cargo-Rail warm | `sccache` 0.16.0 | Warm vs Cargo | Warm vs `sccache` |
|---|---|---:|---:|---:|---:|---:|---:|
| Linux | Check | 9.866 s | 10.994 s | 3.848 s | 4.735 s | 61.0% faster | 18.7% faster |
| Linux | Release build | 12.445 s | 15.119 s | 6.055 s | 7.918 s | 51.3% faster | 23.5% faster |
| Windows | Check | 19.500 s | 22.694 s | 9.744 s | 12.847 s | 50.0% faster | 24.2% faster |
| Windows | Release build | 23.150 s | 28.000 s | 13.579 s | 17.202 s | 41.3% faster | 21.1% faster |

On Linux, the warm check hashed 62,723,112 bytes, read 31,243,330 cache bytes, and restored 30,914,896 output bytes.
The warm release build hashed 82,519,866 bytes, read 82,883,850 cache bytes, and restored 82,538,597 output bytes.
Complete process-tree accounting measured 11.423 CPU-seconds and 374,693,888 bytes peak RSS for the warm check,
compared with 15.287 CPU-seconds and 1,105,412,096 bytes for `sccache`. Release build measured 18.482 CPU-seconds and
386,019,328 bytes for Cargo-Rail, compared with 23.230 CPU-seconds and 1,045,946,368 bytes for `sccache`.

On Windows, both workloads changed 28 cold publications into 28 warm hits while preserving the same 29 explicit
bypasses and exact output manifests. Warm check hashed 54,640,890 unit bytes, read 34,194,373 cache bytes, and restored
33,858,458 output bytes. Warm release build hashed 72,159,810 unit bytes, read 86,337,345 cache bytes, and restored
85,984,609 output bytes. Each Windows Cargo-Rail cold or warm lane also hashed 313,772,556 setup bytes; removing that
repeated identity work is open optimization work. Windows retains wall-time comparisons but not specialist CPU or RSS
claims because the detached `sccache` server is outside the complete accounting boundary.

One observed warm reuse repaid the cold-publication premium in every host/workload pair. The measured repayment was
0.187 check reuses and 0.418 release-build reuses on Linux, and 0.327 and 0.507 on Windows.

The same corpora keep the remaining costs visible. Linux no-op supervision measured -0.045 ms for check and +1.085 ms
for release build; Windows measured -3.579 ms and +4.597 ms. All were within the 7 ms point budget. The explicit
incremental bypass added 346.840/202.603 ms on Linux and 792.636/779.374 ms on Windows. Preserving `sccache` added
6.523/8.245 s on Linux and 11.685/14.875 s on Windows. Those bypass paths exceeded their 50 ms point budgets and remain
optimization work; they do not weaken output-equivalence or false-hit acceptance. A negative point delta means that
the delegated lane happened to finish first in that sample, not that Cargo-Rail made Cargo faster.

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

The benchmark bootstrap stays lean. Before using a live benchmark host as a release qualification runner, install the
pinned check/test tools and run the repository gates explicitly:

```bash
just ssh-qualification-tools aws-linux-x64-bench
just ssh-just aws-linux-x64-bench check
just ssh-just aws-linux-x64-bench build-all
just ssh-just aws-linux-x64-bench test-all
```

Use the matching Windows target for Windows qualification. This profile installs `cargo-nextest`, `cargo-deny`, and
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
bound to its own CPU, operating system, filesystem, toolchain, and target. One complete same-host group supports a
bounded engineering comparison. Repeat the group only when the decision requires tail or variance claims.
