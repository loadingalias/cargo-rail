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
  --slurpfile run "$results/run.json" '
  def quantile($values; $p):
    ($values | sort) as $sorted
    | if ($sorted | length) == 0 then null
      else $sorted[((($sorted | length) - 1) * $p | floor)]
      end;
  def times($samples; $workload; $lane):
    [$samples[] | select(.accepted and .workload == $workload and .lane == $lane) | .measurement.elapsed_seconds];
  def metric($samples; $workload; $lane):
    times($samples; $workload; $lane) as $times
    | {
        workload: $workload,
        lane: $lane,
        accepted_samples: ($times | length),
        p50_elapsed_seconds: quantile($times; 0.50),
        p95_elapsed_seconds: quantile($times; 0.95),
        mean_elapsed_seconds: (if ($times | length) == 0 then null else ($times | add / length) end),
        min_elapsed_seconds: (if ($times | length) == 0 then null else ($times | min) end),
        max_elapsed_seconds: (if ($times | length) == 0 then null else ($times | max) end)
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
  | {
      schema_version: 11,
      evidence_kind: $contract.evidence_kind,
      requested_samples_per_lane: $contract.required_accepted_samples,
      total_samples: ($samples | length),
      accepted_samples: ([$samples[] | select(.accepted)] | length),
      rejected_samples: ([$samples[] | select(.accepted | not)] | length),
      minimum_accepted_samples_per_lane: ($metrics | map(.accepted_samples) | min // 0),
      false_hits: ([$samples[] | select((.lane == "transparent-warm") and (.cache_outcome_valid | not))] | length),
      metrics: $metrics,
      comparisons: $comparisons,
      coverage: {
        check: $check_coverage[0],
        build: $build_coverage[0]
      },
      performance_qualified: (
        $contract.required_accepted_samples >= $contract.qualification.minimum_samples
        and $check_coverage[0].coverage_gate.passed
        and $build_coverage[0].coverage_gate.passed
        and ($comparisons | all(
          .l0_p95_within_bound
          and .cache_off_p95_within_bound
          and .l1_vs_sccache_p50_reduction_percent
            > $contract.qualification.empty_target_l1_vs_sccache_p50_and_p95_min_percent
          and .l1_vs_sccache_p95_reduction_percent
            > $contract.qualification.empty_target_l1_vs_sccache_p50_and_p95_min_percent
        ))
      ),
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
    and .minimum_accepted_samples_per_lane >= $required
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
  performance_qualified,
  comparisons
}' "$results/summary.json"
