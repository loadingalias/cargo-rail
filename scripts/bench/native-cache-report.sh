#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
results="${2:-}"
case "$mode" in
  summarize | validate) ;;
  *)
    echo "usage: $0 <summarize|validate> <result-directory>" >&2
    exit 2
    ;;
esac
results="$(cd "$results" 2>/dev/null && pwd -P)" || {
  echo "native-cache result directory does not exist: $results" >&2
  exit 2
}
for required in environment.json run.json; do
  [[ -f "$results/$required" ]] || {
    echo "native-cache result is missing $required: $results" >&2
    exit 2
  }
done

samples="$results/samples.jsonl"
: >"$samples"
while IFS= read -r -d '' sample; do
  jq -c . "$sample" >>"$samples"
done < <(find "$results/raw" -type f -name sample.json -print0 2>/dev/null | sort -z)

jq -s \
  --slurpfile environment "$results/environment.json" \
  --slurpfile check_coverage "$results/coverage/check/coverage.json" \
  --slurpfile build_coverage "$results/coverage/build/coverage.json" \
  --slurpfile test_coverage "$results/coverage/test/coverage.json" \
  --slurpfile compiler_modes "$results/coverage/compiler-modes/coverage.json" \
  --slurpfile run "$results/run.json" '
  def quantile($values; $p):
    ($values | sort) as $sorted
    | if ($sorted | length) == 0 then null
      else $sorted[((($sorted | length) - 1) * $p | floor)]
      end;
  def metric($samples; $workload; $lane):
    [$samples[] | select(.accepted and .workload == $workload and .lane == $lane)] as $selected
    | [$selected[].measurement.elapsed_seconds] as $times
    | {
        workload: $workload,
        lane: $lane,
        accepted_samples: ($times | length),
        p50_elapsed_seconds: quantile($times; 0.50),
        p95_elapsed_seconds: quantile($times; 0.95),
        mean_elapsed_seconds: (if ($times | length) == 0 then null else ($times | add / length) end),
        min_elapsed_seconds: (if ($times | length) == 0 then null else ($times | min) end),
        max_elapsed_seconds: (if ($times | length) == 0 then null else ($times | max) end),
        total_user_seconds: ([$selected[].measurement.user_seconds | select(. != null)] | add // null),
        total_system_seconds: ([$selected[].measurement.system_seconds | select(. != null)] | add // null),
        max_rss_bytes: ([$selected[].measurement.max_rss_bytes | select(. != null)] | max // null)
      };
  def reduction($baseline; $candidate):
    if $baseline == null or $candidate == null or $baseline == 0 then null
    else 100 * ($baseline - $candidate) / $baseline
    end;
  def overhead_within_bound($baseline; $candidate):
    if $baseline == null or $candidate == null then false
    else $candidate <= ([$baseline * 1.03, $baseline + 0.05] | max)
    end;
  . as $samples
  | $run[0] as $contract
  | [
      $contract.workloads[] as $workload
      | $contract.lanes[] as $lane
      | metric($samples; $workload; $lane)
    ] as $metrics
  | [
      $contract.workloads[] as $workload
      | (metric($samples; $workload; "cargo-l0")) as $cargo_l0
      | (metric($samples; $workload; "cargo-empty")) as $cargo_empty
      | (metric($samples; $workload; "transparent-l0")) as $transparent_l0
      | (metric($samples; $workload; "cache-off")) as $cache_off
      | (metric($samples; $workload; "transparent-cold")) as $cold
      | (metric($samples; $workload; "transparent-warm")) as $warm
      | (metric($samples; $workload; "sccache-server")) as $sccache
      | {
          workload: $workload,
          l0_p50_overhead_percent: reduction($cargo_l0.p50_elapsed_seconds; $transparent_l0.p50_elapsed_seconds) * -1,
          l0_p95_overhead_percent: reduction($cargo_l0.p95_elapsed_seconds; $transparent_l0.p95_elapsed_seconds) * -1,
          cache_off_p50_overhead_percent: reduction($cargo_empty.p50_elapsed_seconds; $cache_off.p50_elapsed_seconds) * -1,
          cache_off_p95_overhead_percent: reduction($cargo_empty.p95_elapsed_seconds; $cache_off.p95_elapsed_seconds) * -1,
          cold_p50_overhead_percent: reduction($cargo_empty.p50_elapsed_seconds; $cold.p50_elapsed_seconds) * -1,
          cold_p95_overhead_percent: reduction($cargo_empty.p95_elapsed_seconds; $cold.p95_elapsed_seconds) * -1,
          l1_vs_sccache_p50_reduction_percent: reduction($sccache.p50_elapsed_seconds; $warm.p50_elapsed_seconds),
          l1_vs_sccache_p95_reduction_percent: reduction($sccache.p95_elapsed_seconds; $warm.p95_elapsed_seconds),
          l0_p95_within_bound: overhead_within_bound($cargo_l0.p95_elapsed_seconds; $transparent_l0.p95_elapsed_seconds),
          cache_off_p95_within_bound: overhead_within_bound($cargo_empty.p95_elapsed_seconds; $cache_off.p95_elapsed_seconds)
        }
    ] as $comparisons
  | [
      $check_coverage[0].operation_inventory.rust_class_counts,
      $build_coverage[0].operation_inventory.rust_class_counts,
      $test_coverage[0].operation_inventory.rust_class_counts
    ]
  | map(to_entries[] | select(.value > 0) | .key)
  | unique as $observed_compiler_classes
  | [
      "binary",
      "build_script",
      "c_dynamic_library",
      "proc_macro_producer",
      "rust_dynamic_library",
      "rust_library",
      "static_library",
      "test"
    ] as $required_compiler_classes
  | ($required_compiler_classes - $observed_compiler_classes) as $missing_compiler_classes
  | ([$samples[] | select(.accepted)] | length) as $accepted_samples
  | ([$samples[] | select(.accepted | not)] | length) as $rejected_samples
  | ([$samples[] | select((.lane == "transparent-warm") and (.cache_outcome_valid | not))] | length) as $false_hits
  | ($metrics | map(.accepted_samples) | min // 0) as $minimum_accepted_samples
  | ([$contract.qualification.empty_target_l1_vs_sccache_p50_and_p95_min_percent, 10] | max) as $superiority_minimum
  | ($contract.qualification.empty_target_l1_vs_sccache_p50_and_p95_target_percent // 15) as $performance_target
  | (
      $contract.evidence_kind == "retained"
      and $contract.required_accepted_samples >= $contract.qualification.minimum_samples
      and $contract.required_accepted_samples <= 30
      and ($metrics | all(.accepted_samples == $contract.required_accepted_samples))
      and $rejected_samples == 0
      and $false_hits == 0
      and $check_coverage[0].coverage_gate.passed
      and $build_coverage[0].coverage_gate.passed
      and $test_coverage[0].coverage_gate.passed
      and $compiler_modes[0].passed
      and ($missing_compiler_classes | length) == 0
    ) as $evidence_qualified
  | ($comparisons | all(.l0_p95_within_bound and .cache_off_p95_within_bound)) as $overhead_within_bounds
  | ($comparisons | all(
      .l1_vs_sccache_p50_reduction_percent >= $superiority_minimum
      and .l1_vs_sccache_p95_reduction_percent >= $superiority_minimum
    )) as $superiority_reached
  | ($comparisons | all(
      .l1_vs_sccache_p50_reduction_percent >= $performance_target
      and .l1_vs_sccache_p95_reduction_percent >= $performance_target
    )) as $target_reached
  | {
      schema_version: 17,
      evidence_kind: $contract.evidence_kind,
      requested_samples_per_lane: $contract.required_accepted_samples,
      total_samples: ($samples | length),
      accepted_samples: $accepted_samples,
      rejected_samples: $rejected_samples,
      minimum_accepted_samples_per_lane: $minimum_accepted_samples,
      false_hits: $false_hits,
      metrics: $metrics,
      comparisons: $comparisons,
      coverage: {
        check: $check_coverage[0],
        build: $build_coverage[0],
        test: $test_coverage[0],
        compiler_modes: $compiler_modes[0]
      },
      compiler_class_coverage: {
        complete: ($missing_compiler_classes | length) == 0,
        required: $required_compiler_classes,
        observed: $observed_compiler_classes,
        missing: $missing_compiler_classes
      },
      overhead_qualified: ($evidence_qualified and $overhead_within_bounds),
      superiority_qualified: ($evidence_qualified and $superiority_reached),
      target_qualified: ($evidence_qualified and $target_reached),
      performance_qualified: ($evidence_qualified and $overhead_within_bounds and $superiority_reached),
      environment: $environment[0],
      contract: $contract
    }
' "$samples" >"$results/summary.json"

if [[ "$mode" == validate ]]; then
  required="$(jq -er '.required_accepted_samples' "$results/run.json")"
  if ! jq -e --argjson required "$required" '
    .rejected_samples == 0
    and .false_hits == 0
    and .coverage.check.coverage_gate.passed
    and .coverage.build.coverage_gate.passed
    and .coverage.test.coverage_gate.passed
    and .coverage.compiler_modes.passed
    and .compiler_class_coverage.complete
    and (.metrics | all(.accepted_samples == $required))
  ' "$results/summary.json" >/dev/null; then
    echo "native-cache benchmark did not retain the requested accepted samples" >&2
    jq '{requested_samples_per_lane, accepted_samples, rejected_samples, false_hits, minimum_accepted_samples_per_lane}' \
      "$results/summary.json" >&2
    exit 1
  fi
fi

jq '{
  evidence_kind,
  requested_samples_per_lane,
  accepted_samples,
  rejected_samples,
  false_hits,
  minimum_accepted_samples_per_lane,
  overhead_qualified,
  superiority_qualified,
  target_qualified,
  performance_qualified,
  compiler_class_coverage,
  comparisons
}' "$results/summary.json"
