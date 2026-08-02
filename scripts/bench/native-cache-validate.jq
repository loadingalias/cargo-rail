def stable_cache_outcome:
  if .kind == "cargo-rail-native"
  then {
    status,
    reason,
    hits,
    misses,
    bypasses,
    reasons,
    setup_bytes_hashed,
    unit_bytes_hashed,
    cache_bytes_read,
    cache_bytes_written,
    bytes_restored,
    cache_directory_present,
    specialist_outcome: (.specialist.outcome_digest // null)
  }
  else {outcome_digest}
  end;

group_by([.workload, .lane])
| map(
    sort_by(.round) as $lane
    | $lane[0] as $reference
    | $lane
    | map(
        (.cache | stable_cache_outcome) as $outcome
        | ($reference.cache | stable_cache_outcome) as $reference_outcome
        | (
            .outputs.digest == $reference.outputs.digest
            and .action_census.digest == $reference.action_census.digest
            and .runtime.identity == $reference.runtime.identity
            and .cache.eligible_unit_digest == $reference.cache.eligible_unit_digest
            and $outcome == $reference_outcome
          ) as $equivalent
        | .accepted = (.accepted and $equivalent)
        | .repeated_sample_equivalence = {
            reference_sample_id: $reference.sample_id,
            output_manifest_identical: (.outputs.digest == $reference.outputs.digest),
            action_census_identical: (.action_census.digest == $reference.action_census.digest),
            runtime_output_identical: (.runtime.identity == $reference.runtime.identity),
            eligible_units_identical: (.cache.eligible_unit_digest == $reference.cache.eligible_unit_digest),
            outcomes_identical: ($outcome == $reference_outcome),
            accepted: $equivalent
          }
        | if $equivalent then .
          else .group_validity.rejection_reasons += [
            if .outputs.digest == $reference.outputs.digest then empty else "lane_output_manifest_changed" end,
            if .action_census.digest == $reference.action_census.digest then empty else "lane_action_census_changed" end,
            if .runtime.identity == $reference.runtime.identity then empty else "lane_runtime_output_changed" end,
            if .cache.eligible_unit_digest == $reference.cache.eligible_unit_digest
              then empty else "lane_eligible_units_changed"
            end,
            if $outcome == $reference_outcome then empty else "lane_outcomes_changed" end
          ]
          end
      )
  )
| flatten
| sort_by([.round, .workload, .interleave_position])
| .[]
