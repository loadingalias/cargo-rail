# Benchmarking

Cargo-Rail has one cache benchmark.
It compares native Cargo, Cargo-Rail, and sccache on the bundled mixed Rust/native workload.
The local command covers cold builds, empty-target rebuilds, Cargo freshness, and a source edit.
It does not benchmark remote storage, distributed execution, or arbitrary user workspaces.
Cargo-Rail reports verified restored files.
File attribution is not available for sccache.

A performance claim needs retained correctness evidence from the exact source, binary, toolchain,
host, and workload that were measured.
A smoke run validates orchestration.
It does not qualify latency.

## Run the local comparison

1. Install Hyperfine and sccache.
1. From this source checkout, run:

   ```bash
   just bench
   ```

This command builds the matching release components and runs the benchmark.
The benchmark executable embeds its workload.
It also runs outside the checkout:

```bash
cargo install --path . --locked --features bench --bin cargo-rail-bench
cargo rail-bench local --rail /path/to/cargo-rail/target/release/cargo-rail
```

The selected Cargo-Rail executable needs its matching authenticated cache components beside it.
For a source comparison, build the benchmark and cache components from the same revision.
With a matching installation, `cargo rail-bench` uses its sibling `cargo-rail` first, and then searches `PATH`.

By default, the benchmark runs five samples for each tool in each of the four local scenarios.
It uses the ordinary Cargo dev profile and default parallelism,
with incremental compilation enabled.
`--non-incremental` selects a controlled comparison with a separate label.
That option applies equally to every tool.
Examples:

```bash
cargo rail-bench local --non-incremental
cargo rail-bench local --scenario rebuild,source-edit --runs 3
cargo rail-bench local --smoke
```

`--smoke` runs one sample for each case and marks all timings as unqualified.
Use `--smoke` to validate orchestration.
Collect performance results on a quiet machine.
The current local runner requires Unix socket isolation for its private sccache daemon.
Thus, the local timing command rejects hosts other than Unix, such as Windows.
Workload preparation is portable.

Use `--rail PATH` to select the Cargo-Rail executable.
By default, the benchmark writes its results to a private temporary directory and retains
that directory.
`--output PATH` selects a new directory.
The parent of that directory must exist.
The benchmark refuses an existing output directory.
Select a location that has no Cargo configuration or toolchain files in its ancestor directories.

Each tool gets its own Cargo home, configuration, and cache.
The runner downloads the frozen workload dependencies before timing.
It clears ambient cache and compiler controls.
It captures the selected toolchain and the executable digests, and it refuses changed executables.
It never cleans the user's workspace or active cache.
Each sample resets the target and result-cache state.
Then the sample prepares its scenario outside timing:

| Scenario      | State at the timed build |
| ------------- | ------------------------ |
| `cold`        | Empty target and result cache; no seed build |
| `rebuild`     | Seeded result cache; seed target removed |
| `freshness`   | Seeded result cache and target retained |
| `source-edit` | Seeded state retained; source changed to produce `120` instead of `119` |

The private sccache daemon starts before timing and stops after each sample.

Hyperfine times Cargo directly, without a shell, for one sample at a time.
The tool order rotates each round.
These steps are outside timing: downloads, cache reset, seeding, benchmark evidence hashing,
and executable-output checks.
Compiler and cache input hashing stays inside the measured Cargo build.
Measured builds enable Cargo JSON output and Cargo-Rail per-action output evidence.
The timings include that reporting overhead.
The runner does not flush OS file caches.

The result directory retains `summary.json`, a private `provenance.json`, and one evidence directory for each sample.
Evidence includes raw Hyperfine JSON, Cargo logs, cache statistics, Cargo-reported file digests,
and executable output.
A failure stops the run.
The run then retains the completed and pending rows, and it retains `failure.txt`.
A successful summary means that the selected local checks passed.
It does not qualify other hosts.
It does not prove that all cached artifacts are correct.

The text report shows elapsed-time medians, ranges, and sample counts.
It also shows aggregate compiler hits, verified Cargo-Rail restored files, logical bytes,
and output roles.
Failed compiler attempts stay separate from Cargo build success and from cache I/O failures.
Failed compiler attempts can include rejected capability probes from builds that otherwise succeed.
`verified_restored_files: null` means that attribution is not available.
It does not mean zero.
The complete output inventory of each Cargo-Rail hit must match an admission from the seed of
that sample.
The headline count includes the distinct restored paths whose bytes and modes still match
when the build completes.
Deleted probe outputs and overwritten intermediates stay in the events and in `unretained_restore_outputs`.
They are not added to the headline count.
A restored output that the seed retained must not disappear.
Raw events keep repeated actions.
The distinct-file total counts a repeated destination one time.
Hit and miss totals must also agree with the usage counters
that the installation records independently.
Recording identities reject events copied from another sample.
These checks do not give comparable file attribution for sccache.
They do not support a claim of better artifact coverage.
Cargo-reported file inventories include executables without an extension.
These inventories are not the complete compiler-output inventory, and they do not prove restoration.
Review private logs before you share them.
The benchmark does not upload anything automatically.

## Prepare the workload

To create the workload without compiling or timing, run:

```bash
cargo rail-bench prepare --output /path/to/new-workspace
```

The output directory must not exist.
Its parent must exist.
Preparation creates that workspace and a sibling Git dependency directory named `new-workspace.git-source`.
The command embeds the mixed Rust/native workload and a frozen registry dependency graph.
Preparation then prefetches the locked dependencies into the selected Cargo home.
Git and Cargo must be available.
Bash is not required.

Use `--offline` to require cached registry packages.
The command still prefetches the bundled local Git source.
Use `--git-source PATH` to share that exact bundled Git revision between prepared workspaces.
The command refuses existing output directories and mismatched Git sources.
If preparation fails, it retains the partial output for diagnosis.
This preparation command keeps the controlled profiles of the correctness fixture.
The local comparison selects ordinary dev settings separately.
The benchmark binary requires the `bench` feature.
Release archives do not include the benchmark binary.
[Release packaging and publication](releases.md) have separate evidence requirements.
Benchmark results do not authorize a release.

## Measurement rules

1. Compare equivalent workloads and settings on the same quiet machine.
   Keep ordinary Cargo incremental compilation.
   Label controlled non-incremental runs separately, and apply that setting equally to every tool.
1. Bind the source state, executable and component digests, workload and lockfile,
   toolchain, host, target, arguments, concurrency, instrumentation, and starting
   cache state.
1. Reset the declared state before every sample.
   Keep preparation, downloads, seed builds,
   and output validation outside the timed client-visible Cargo build.
1. Retain raw samples, failures, outliers, and execution order.
   Failed or incomplete evidence blocks the related claim.
   That evidence must not disappear from the report.
1. Report elapsed time and sample variability first.
   One sample is a point measurement.
   A small sample set does not support tail-percentile claims.

Do not combine different source states, toolchains, target-state policies, physical roots,
cache protocols, or machines into one population.
Do not compare timings that were collected while unrelated builds or tests ran.

## Locate a measured cost

Retain an untraced baseline.
Then profile a repeatable slow scenario.
Include the complete process tree: Cargo, wrapper, compiler, and worker.
A profile of only the parent process can miss the work that takes most of the time in a cold build.
Before you change code, separate CPU work, file reads, synchronization, and process startup.

Keep diagnostic instrumentation separate from timing evidence.
Rebuild the final components without that instrumentation.
Then repeat the same cold and warm scenarios.
Report remaining overhead and inconclusive results together with gains.
A reduction in hashed bytes does not prove a decrease in elapsed time.

### Linux perf

The Linux `ci` tooling operation installs perf for the running kernel,
including its distribution flavor.
Before you collect a profile, make sure that `perf --version` works.
Kernel permissions and available hardware counters determine which events the host can record.

First, prepare the fixture, dependencies, compiler components, and intended cold or warm state.
Run the measured Cargo command under `perf record` with `--no-buildid-cache`.
The default build-ID cache of perf can create hard links to installed worker executables.
These links violate the single-link ownership checks of Cargo-Rail and change the behavior
that you profile.
Keep the original component binaries and symbols with the profile for later analysis.

Record inherited child processes and call stacks.
Retain the exact event selection, sampling frequency, buffer size, and lost-sample count.
Confirm the cache outcomes and fixture output again for the profiled build.
A successful compiler exit alone does not prove that the intended hit or miss path ran.
Use unprofiled runs for the before/after timing claim.

## Reuse and artifact counts

Report Cargo-fresh units, reused compiler actions, and verified restored artifact files separately.
A hit is an action outcome, not a count of restored files.
A successful build or matching output bytes alone cannot distinguish compilation from restoration.

Count restored files only when both of these conditions are true:

- their complete inventory, bytes, and modes match the admitted seed result;
- each file is attributed to a verified restore action.

Keep logical restored bytes separate from compressed transfer bytes.
Show output classes.
Use the common workload output inventory as the denominator, not the eligible subset of each tool.
Report unavailable attribution as unavailable.

Validate behavior against an uncached reference.
Independent builds can produce different bytes.
Compare exact restore fidelity against the admitted result of that tool.
Do not normalize arbitrary differences.
Worker compilation is remote execution, not a result-cache hit.
Remote fallback is not successful remote reuse.

## Claim requirements

Publish a performance claim only when:

- the retained result passes the correctness and execution-mode checks that the exact claim needs;
- the claim states the workload, host, target, toolchain, settings, commands, sample count, and
  measured values;
- each timed sample has its own evidence, and the complete corpus stays available; and
- the claim is limited to the measured platform and workload.

A claim that one tool restores more artifacts also requires comparable attribution for both tools.
If file attribution is missing, a timing comparison can still be valid.
Missing attribution cannot count as zero restored files or as better artifact coverage.
More restored artifacts do not mean a faster build.
Report losses and inconclusive differences directly.

Keep failed and incomplete rows visible with their evidence paths.
Share summaries without credentials or private source.
Detailed logs, paths, and diagnostics can need review before you share them.
The benchmark never uploads reports automatically.
