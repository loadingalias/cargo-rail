#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mode="${1:-}"
results="${2:-}"

case "$mode" in
  summarize | validate) ;;
  *)
    echo "usage: $0 <summarize|validate> <result-directory>" >&2
    exit 2
    ;;
esac
[[ -n "$results" ]] || {
  echo "native-cache result directory is required" >&2
  exit 2
}
results="$(cd "$results" 2>/dev/null && pwd -P)" || {
  echo "native-cache result directory does not exist: $results" >&2
  exit 2
}

for tool in find jq sort; do
  command -v "$tool" >/dev/null || {
    echo "missing required benchmark report tool: $tool" >&2
    exit 2
  }
done
for required in environment.json run.json; do
  [[ -f "$results/$required" ]] || {
    echo "native-cache result is missing $required: $results" >&2
    exit 2
  }
done

temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-native-report.XXXXXX")"
trap 'rm -rf -- "$temporary"' EXIT
raw_samples="$temporary/raw-samples.jsonl"
: >"$raw_samples"

while IFS= read -r -d '' group; do
  jq -c '
    . as $group
    | .samples[]
    | . + {
        accepted: $group.accepted,
        group_validity: {
          complete: $group.complete,
          output_manifests_identical: $group.output_manifests_identical,
          cross_root_artifacts: $group.cross_root_artifacts,
          runtime_outputs_identical: $group.runtime_outputs_identical,
          action_census_identical: $group.action_census_identical,
          bypass_cache_io_zero: $group.bypass_cache_io_zero,
          cache_event_streams_complete: $group.cache_event_streams_complete,
          cache_proof_replays_valid: $group.cache_proof_replays_valid,
          cache_equivalent: $group.cache_equivalent,
          cache_io_consistent: $group.cache_io_consistent,
          rejection_reasons: $group.rejection_reasons
        }
      }
  ' "$group" >>"$raw_samples"
done < <(find "$results/raw" -type f -name group.json -print0 2>/dev/null | sort -z)

samples="$results/samples.jsonl"
invalid_samples="$results/invalid-samples.jsonl"
event_diffs="$results/compiler-event-diffs.json"
jq -c -s -f "$repo_root/scripts/bench/native-cache-validate.jq" "$raw_samples" >"$samples"
jq -c 'select(.accepted | not)' "$samples" >"$invalid_samples"
jq -s '
  group_by([.workload, .lane])
  | map(
      sort_by(.round) as $lane
      | $lane[0] as $reference
      | $lane[]
      | select(.repeated_sample_equivalence.accepted | not)
      | {
          sample_id,
          workload,
          lane,
          reference_sample_id: $reference.sample_id,
          reference_event_count: (($reference.cache.events // []) | length),
          sample_event_count: ((.cache.events // []) | length),
          eligible_units_added: ((.cache.eligible_units // []) - ($reference.cache.eligible_units // [])),
          eligible_units_removed: (($reference.cache.eligible_units // []) - (.cache.eligible_units // [])),
          events_added: ((.cache.events // []) - ($reference.cache.events // [])),
          events_removed: (($reference.cache.events // []) - (.cache.events // [])),
          output_manifest_identical: .repeated_sample_equivalence.output_manifest_identical,
          action_census_identical: .repeated_sample_equivalence.action_census_identical,
          runtime_output_identical: .repeated_sample_equivalence.runtime_output_identical,
          eligible_units_identical: .repeated_sample_equivalence.eligible_units_identical,
          outcomes_identical: .repeated_sample_equivalence.outcomes_identical
        }
    )
  | flatten
' "$samples" >"$event_diffs"

optional_json() {
  local path="$1"
  local fallback="$2"
  if [[ -f "$path" ]]; then
    printf '%s\n' "$path"
  else
    local temporary_path
    temporary_path="$temporary/$(basename "$path")"
    printf '%s\n' "$fallback" >"$temporary_path"
    printf '%s\n' "$temporary_path"
  fi
}

runs="$(jq -er '.required_accepted_samples' "$results/run.json")"
attempts="$(jq -er '.attempts' "$results/run.json")"
warmup_runs="$(jq -r '.warmup_runs // if .evidence_kind == "smoke" then 0 else 1 end' "$results/run.json")"
evidence_kind="$(jq -er '.evidence_kind' "$results/run.json")"
sccache_available="$(jq -r '.sccache.available' "$results/run.json")"
[[ "$sccache_available" == true || "$sccache_available" == false ]] || {
  echo "native-cache run metadata has an invalid sccache availability value" >&2
  exit 2
}
sccache_path="$(jq -r '.sccache.path // ""' "$results/run.json")"
sccache_version="$(jq -r '.sccache.version // ""' "$results/run.json")"
sccache_unavailable_reason="$(jq -r '.sccache.unavailable_reason // ""' "$results/run.json")"
check_units="$(optional_json "$results/unit-check.json" '{"available":false,"reason":"unit timing has not completed"}')"
build_units="$(optional_json "$results/unit-build.json" '{"available":false,"reason":"unit timing has not completed"}')"
check_seed="$(optional_json "$results/seed-check-cache-status.json" 'null')"
build_seed="$(optional_json "$results/seed-build-cache-status.json" 'null')"

jq -s \
  --argjson runs "$runs" \
  --argjson attempts "$attempts" \
  --argjson warmup_runs "$warmup_runs" \
  --arg evidence_kind "$evidence_kind" \
  --argjson sccache_available "$sccache_available" \
  --arg sccache_path "$sccache_path" \
  --arg sccache_version "$sccache_version" \
  --arg sccache_unavailable_reason "$sccache_unavailable_reason" \
  --slurpfile environment "$results/environment.json" \
  --slurpfile run "$results/run.json" \
  --slurpfile check_units "$check_units" \
  --slurpfile build_units "$build_units" \
  --slurpfile check_seed "$check_seed" \
  --slurpfile build_seed "$build_seed" \
  -f "$repo_root/scripts/bench/native-cache-summary.jq" \
  "$samples" >"$results/summary.json"

if [[ "$mode" == summarize ]]; then
  jq '{evidence_kind, publishable, runs, attempts, accepted_samples, rejected_samples, validity}' \
    "$results/summary.json"
  exit 0
fi

if ! jq -e --argjson required "$runs" '
  .validity.measurement_complete
  and .validity.minimum_accepted_samples_per_lane >= $required
  and .validity.false_hits == 0
' "$results/summary.json" >/dev/null; then
  echo "native-cache benchmark has not retained $runs equivalent samples in every available lane" >&2
  exit 1
fi

if [[ "$runs" -lt 1 ]]; then
  echo "native-cache evidence requires at least one accepted sample per lane" >&2
  exit 1
fi

jq '{evidence_kind, publishable, runs, attempts, accepted_samples, rejected_samples, validity}' \
  "$results/summary.json"
