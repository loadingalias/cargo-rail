# Benchmarking

Cargo-Rail has one cache benchmark: native Cargo,
Cargo-Rail and sccache on the bundled mixed Rust/native workload.
The local command covers cold builds, empty-target rebuilds, Cargo freshness and a source edit.
It does not benchmark remote storage, distributed execution or arbitrary user workspaces.
Cargo-Rail reports verified restored files; sccache file attribution is unavailable.

A performance claim needs retained correctness evidence from the exact measured source, binary,
toolchain, host and workload.
A smoke run validates orchestration; it does not qualify latency.

## Run the local comparison

Install Hyperfine and sccache.
From this source checkout, run:

```bash
just bench
```

This builds the matching release components and invokes the single benchmark.
The benchmark executable embeds its workload and also runs outside the checkout:

```bash
cargo install --path . --locked --bin cargo-rail-bench
cargo rail-bench local --rail /path/to/cargo-rail/target/release/cargo-rail
```

The selected Cargo-Rail executable needs its matching authenticated cache components beside it.
For a source comparison, build the benchmark and cache components from the same revision.
With a matching installation, `cargo rail-bench` uses its sibling `cargo-rail`, then searches `PATH`.

The default runs five samples per tool for each of the four local scenarios.
It uses Cargo's ordinary dev profile and default parallelism, with incremental compilation enabled.
`--non-incremental` selects a separately labeled controlled comparison and applies equally to every tool:

```bash
cargo rail-bench local --non-incremental
cargo rail-bench local --scenario rebuild,source-edit --runs 3
cargo rail-bench local --smoke
```

`--smoke` exercises one sample per case and marks all timings unqualified.
Use it while validating orchestration; collect performance results on a quiet machine.
The current local runner requires Unix socket isolation for its private sccache daemon;
the local timing command rejects Windows.
Workload preparation is portable.

Use `--rail PATH` to select the Cargo-Rail executable.
The default result is a retained private temporary directory.
`--output PATH` selects a new directory whose parent exists; existing output is refused.
Choose a location outside trees with ancestor Cargo configuration or toolchain files.

Each tool gets its own Cargo home, configuration and cache.
The runner downloads the frozen workload dependencies before timing.
It clears ambient cache and compiler controls,
captures the selected toolchain and executable digests, and refuses changed executables.
It never cleans the user's workspace or active cache.
Each sample resets target and result-cache state, then prepares its scenario outside timing:

| Scenario      | State at the timed build |
| ------------- | ------------------------ |
| `cold`        | Empty target and result cache; no seed build |
| `rebuild`     | Seeded result cache; seed target removed |
| `freshness`   | Seeded result cache and target retained |
| `source-edit` | Seeded state retained; source changed to produce `120` instead of `119` |

The private sccache daemon starts before timing and stops after each sample.

Hyperfine times Cargo directly, without a shell, for one sample at a time.
Comparator order rotates each round.
Downloads, cache reset, seeding,
benchmark evidence hashing and executable-output checks are outside timing.
Compiler and cache input hashing remains inside the measured Cargo build.
Cargo JSON output and Cargo-Rail per-action output evidence are enabled during measured builds;
that reporting overhead is included.
OS file caches are not flushed.

The result directory retains `summary.json`, private `provenance.json`, and one evidence directory per sample.
Evidence includes raw Hyperfine JSON, Cargo logs, cache statistics,
Cargo-reported file digests and executable output.
Failures stop the run, preserve completed and pending rows, and retain `failure.txt`.
A successful summary means the selected local checks passed;
it does not qualify other hosts or prove all cached artifacts correct.

The text report shows elapsed-time medians, ranges, sample counts and aggregate compiler hits,
verified Cargo-Rail restored files, logical bytes and output roles.
Failed compiler attempts, which can include rejected capability probes from otherwise successful
builds, remain separate from Cargo build success and cache I/O failures.
`verified_restored_files: null` means attribution is unavailable, not zero.
Each Cargo-Rail hit's complete output inventory must match an admission from that sample's seed.
The headline count includes distinct restored paths whose bytes
and modes still match at build completion.
Deleted probe outputs or overwritten intermediates remain in the events and `unretained_restore_outputs`;
they do not inflate the headline count.
A restored output retained in the seed must not disappear.
Raw events preserve repeated actions; the distinct-file total counts a repeated destination once.
Hit and miss totals must also agree with the installation's independently recorded usage counters.
Recording identities reject events copied from another sample.
This does not supply comparable file attribution for sccache
or support an artifact-coverage superiority claim.
Cargo-reported file inventories include extensionless executables
but are not the complete compiler-output inventory or proof of restoration.
Review private logs before sharing them; nothing is uploaded automatically.

## Prepare the workload

To materialize without compiling or timing:

```bash
cargo rail-bench prepare --output /path/to/new-workspace
```

The output directory must not exist and its parent must exist.
Preparation creates that workspace and a sibling Git dependency directory named `new-workspace.git-source`.
It embeds the mixed Rust/native workload and a frozen registry dependency graph,
then prefetches the locked dependencies into the selected Cargo home.
Git and Cargo must be available; Bash is not required.

Use `--offline` to require cached registry packages.
The bundled local Git source is still prefetched.
Use `--git-source PATH` to share that exact bundled Git revision between prepared workspaces.
Existing output directories and mismatched Git sources are refused.
Failed preparation retains partial output for diagnosis.
This preparation command preserves the correctness fixture's controlled profiles;
the local comparison selects ordinary dev settings separately.
The benchmark binary is not included in release archives.

## Measurement rules

1. Compare equivalent workloads and settings on the same quiet machine.
   Preserve ordinary Cargo incremental compilation;
   label controlled non-incremental runs separately and apply that setting equally to every tool.
1. Bind source state, executable/component digests, workload and lockfile,
   toolchain, host, target, arguments, concurrency, instrumentation and starting
   cache state.
1. Reset the declared state before every sample.
   Keep preparation, downloads, seed builds
   and output validation outside the timed client-visible Cargo build.
1. Retain raw samples, failures, outliers and execution order.
   Failed or incomplete evidence prevents the associated claim;
   it must not disappear from the report.
1. Report elapsed time and sample variability first.
   One sample is a point measurement; a small sample set does not support tail-percentile claims.

Do not combine different source states, toolchains, target-state policies, physical roots,
cache protocols or machines into one population.
Do not compare timings collected alongside unrelated builds or tests.

## Locate a measured cost

Profile a repeatable slow scenario after retaining an untraced baseline.
Include the complete Cargo, wrapper, compiler and worker process tree:
a parent-process profile alone can miss the work that dominates a cold build.
Separate CPU work, file reads, synchronization and process startup before changing code.

Keep diagnostic instrumentation separate from timing evidence.
Rebuild the final components without that instrumentation and repeat the same cold
and warm scenarios.
Report remaining overhead and inconclusive results alongside gains;
a reduction in bytes hashed does not establish an elapsed-time improvement.

### Linux perf

The Linux `ci` tooling operation installs perf for the running kernel,
including its distribution flavor.
Confirm `perf --version` before collecting a profile.
Kernel permissions and available hardware counters determine which events the host can record.

Prepare the fixture, dependencies, compiler components, and intended cold or warm state first.
Run the measured Cargo command under `perf record` with `--no-buildid-cache`.
Perf's default build-ID cache can create hard links to installed worker executables,
violating Cargo-Rail's single-link ownership checks and changing the behavior being profiled.
Keep the original component binaries and symbols with the profile for later analysis.

Record inherited child processes and call stacks; retain the exact event selection,
sampling frequency, buffer size, and lost-sample count.
Confirm cache outcomes and fixture output again for the profiled build.
A successful compiler exit alone does not prove that the intended hit or miss path ran.
Use unprofiled runs for the before/after timing claim.

## Reuse and artifact counts

Report Cargo-fresh units, reused compiler actions and verified restored artifact files separately.
A hit is an action outcome, not a count of restored files.
A successful build or matching output bytes alone cannot distinguish compilation from restoration.

Credit restored files only when their complete inventory,
bytes and modes match the admitted seed result
and each file is attributed to a verified restore action.
Keep logical restored bytes separate from compressed transfer bytes.
Show output classes and use the common workload output inventory as the denominator,
rather than each tool's eligible subset.
Report unavailable attribution as unavailable.

Validate behavior against an uncached reference.
Independent builds can produce different bytes;
compare exact restore fidelity against
that tool's admitted result without normalizing arbitrary differences.
Worker compilation is remote execution, not a result-cache hit.
Remote fallback is not successful remote reuse.

## Claim requirements

Publish a performance claim only when:

- the retained result passes the correctness and execution-mode checks needed
  for that exact claim;
- workload, host, target, toolchain, settings, commands, sample count and
  measured values are stated;
- each timed sample has its own evidence and the complete corpus remains
  available; and
- the claim is limited to the measured platform and workload.

Artifact-count superiority additionally requires comparable attribution for both tools.
Missing file attribution can leave a timing comparison valid,
but cannot become zero restored files or an artifact-coverage win.
More restored artifacts do not imply a faster build.
Report losses and inconclusive differences directly.

Keep failed and incomplete rows visible with their evidence paths.
Share summaries without credentials or private source; detailed logs,
paths and diagnostics may need review before sharing.
Benchmark reports are never uploaded automatically.
