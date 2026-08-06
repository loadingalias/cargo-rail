def finite_number:
  type == "number" and isfinite;

def percentile($values; $fraction):
  ($values | sort) as $sorted
  | if ($sorted | length) == 0 then null
    else $sorted[(((($sorted | length) * $fraction) | ceil) - 1)]
    end;

# Exact binomial order-statistic bounds avoid a bootstrap model and random seed.
# Each reported side has at least the stated one-sided coverage under independent
# repetitions of the preregistered, round-interleaved experiment.
def binomial_coefficient($count; $selected):
  if $selected < 0 or $selected > $count then 0
  else ([$selected, $count - $selected] | min) as $short
    | reduce range(1; $short + 1) as $index (
        1;
        . * ($count - $short + $index) / $index
      )
  end;

def binomial_cdf($count; $probability; $maximum):
  if $maximum < 0 then 0
  elif $maximum >= $count then 1
  else reduce range(0; $maximum + 1) as $selected (
      0;
      .
        + binomial_coefficient($count; $selected)
        * pow($probability; $selected)
        * pow(1 - $probability; $count - $selected)
    )
  end;

def lower_order_rank($count; $quantile; $confidence):
  [
    range(1; $count + 1) as $rank
    | select(binomial_cdf($count; $quantile; $rank - 1) <= 1 - $confidence + 1e-12)
    | $rank
  ]
  | if length == 0 then null else max end;

def upper_order_rank($count; $quantile; $confidence):
  [
    range(1; $count + 1) as $rank
    | select(binomial_cdf($count; $quantile; $rank - 1) >= $confidence - 1e-12)
    | $rank
  ]
  | if length == 0 then null else min end;

def distribution_free_bounds($values; $quantile; $confidence):
  $values as $raw
  | ($raw | map(select(finite_number)) | sort) as $sorted
  | ($sorted | length) as $count
  | lower_order_rank($count; $quantile; $confidence) as $lower_rank
  | upper_order_rank($count; $quantile; $confidence) as $upper_rank
  | {
      method: "exact_binomial_order_statistic",
      quantile: $quantile,
      one_sided_confidence: $confidence,
      sample_count: $count,
      invalid_sample_count: (($raw | length) - $count),
      estimate: percentile($sorted; $quantile),
      lower_rank: $lower_rank,
      lower_bound: (if $lower_rank == null then null else $sorted[$lower_rank - 1] end),
      lower_bound_coverage: (
        if $lower_rank == null then null
        else 1 - binomial_cdf($count; $quantile; $lower_rank - 1)
        end
      ),
      upper_rank: $upper_rank,
      upper_bound: (if $upper_rank == null then null else $sorted[$upper_rank - 1] end),
      upper_bound_coverage: (
        if $upper_rank == null then null
        else binomial_cdf($count; $quantile; $upper_rank - 1)
        end
      ),
      complete: (
        ($raw | length) == $count
        and $count > 0
        and $lower_rank != null
        and $upper_rank != null
      )
    };

def lane_stats($samples; $workload; $lane):
  [$samples[] | select(.accepted and .available and .workload == $workload and .lane == $lane)] as $lane_samples
  | [$lane_samples[] | select(.resources.accounting.cpu_time.complete)] as $cpu_samples
  | [$lane_samples[] | select(.resources.accounting.peak_rss.complete)] as $rss_samples
  | {
      accepted_samples: ($lane_samples | length),
      rejected_samples: (
        [$samples[] | select(.available and (.accepted | not) and .workload == $workload and .lane == $lane)] | length
      ),
      p50_seconds: percentile([$lane_samples[].resources.wall_seconds]; 0.50),
      p95_seconds: percentile([$lane_samples[].resources.wall_seconds]; 0.95),
      user_p50_seconds: percentile([$cpu_samples[].resources.user_seconds]; 0.50),
      user_p95_seconds: percentile([$cpu_samples[].resources.user_seconds]; 0.95),
      system_p50_seconds: percentile([$cpu_samples[].resources.system_seconds]; 0.50),
      system_p95_seconds: percentile([$cpu_samples[].resources.system_seconds]; 0.95),
      cpu_p50_seconds: percentile([$cpu_samples[].resources.cpu_seconds]; 0.50),
      cpu_p95_seconds: percentile([$cpu_samples[].resources.cpu_seconds]; 0.95),
      user_mean_seconds: (
        [$cpu_samples[].resources.user_seconds] | if length == 0 then null else add / length end
      ),
      system_mean_seconds: (
        [$cpu_samples[].resources.system_seconds] | if length == 0 then null else add / length end
      ),
      peak_rss_bytes: (
        [$rss_samples[].resources.peak_rss_bytes] | if length == 0 then null else max end
      ),
      resource_accounting: {
        wall_time_complete: ($lane_samples | length > 0 and all(.resources.accounting.wall_time.complete)),
        cpu_time_complete: ($lane_samples | length > 0 and all(.resources.accounting.cpu_time.complete)),
        peak_rss_complete: ($lane_samples | length > 0 and all(.resources.accounting.peak_rss.complete)),
        cpu_time_scope: ($lane_samples | map(.resources.accounting.cpu_time.scope) | unique),
        cpu_time_unavailable_reasons: (
          $lane_samples | map(.resources.accounting.cpu_time.reason // empty) | unique
        ),
        peak_rss_scope: ($lane_samples | map(.resources.accounting.peak_rss.scope) | unique),
        peak_rss_unavailable_reasons: (
          $lane_samples | map(.resources.accounting.peak_rss.reason // empty) | unique
        )
      },
      cache: {
        setup_bytes_hashed_p50: percentile([$lane_samples[].cache.setup_bytes_hashed]; 0.50),
        setup_bytes_hashed_p95: percentile([$lane_samples[].cache.setup_bytes_hashed]; 0.95),
        unit_bytes_hashed_p50: percentile([$lane_samples[].cache.unit_bytes_hashed]; 0.50),
        unit_bytes_hashed_p95: percentile([$lane_samples[].cache.unit_bytes_hashed]; 0.95),
        cache_bytes_read_p50: percentile([$lane_samples[].cache.cache_bytes_read]; 0.50),
        cache_bytes_read_p95: percentile([$lane_samples[].cache.cache_bytes_read]; 0.95),
        cache_bytes_written_p50: percentile([$lane_samples[].cache.cache_bytes_written]; 0.50),
        cache_bytes_written_p95: percentile([$lane_samples[].cache.cache_bytes_written]; 0.95),
        bytes_restored_p50: percentile([$lane_samples[].cache.bytes_restored]; 0.50),
        bytes_restored_p95: percentile([$lane_samples[].cache.bytes_restored]; 0.95)
      },
      acquisitions: {
        cargo_metadata_loads_p50: percentile([$lane_samples[].execution.diagnostics.cargo_metadata_loads]; 0.50),
        cargo_metadata_loads_p95: percentile([$lane_samples[].execution.diagnostics.cargo_metadata_loads]; 0.95),
        target_view_loads_p50: percentile([$lane_samples[].execution.diagnostics.target_view_loads]; 0.50),
        target_view_loads_p95: percentile([$lane_samples[].execution.diagnostics.target_view_loads]; 0.95),
        git_subprocesses_p50: percentile([$lane_samples[].execution.diagnostics.git_subprocesses]; 0.50),
        git_subprocesses_p95: percentile([$lane_samples[].execution.diagnostics.git_subprocesses]; 0.95),
        compiler_processes_p50: percentile([$lane_samples[].action_census.count]; 0.50),
        compiler_processes_p95: percentile([$lane_samples[].action_census.count]; 0.95)
      }
    };

def publication_repayment($samples; $workload):
  lane_stats($samples; $workload; "native-cargo") as $native
  | lane_stats($samples; $workload; "cargo-rail-cold") as $cold
  | lane_stats($samples; $workload; "cargo-rail-warm") as $warm
  | if $native.p50_seconds == null or $cold.p50_seconds == null or $warm.p50_seconds == null
      or $native.p50_seconds <= $warm.p50_seconds
    then null
    else ([($cold.p50_seconds - $native.p50_seconds), 0] | max)
      / ($native.p50_seconds - $warm.p50_seconds)
    end;

def false_hits($samples):
  [
    ($samples | group_by([.workload, .round])[]) as $group
    | ($group | map({key: .lane, value: .}) | from_entries) as $by_lane
    | $group[]
    | select(.available and .cache.kind == "cargo-rail-native" and ((.cache.hits // 0) > 0))
    | select(
        ($by_lane["cargo-rail-cold"].outputs.available | not)
        or (.outputs.available | not)
        or $by_lane["cargo-rail-cold"].outputs.digest != .outputs.digest
        or (
          .workload == "build"
          and (
            ($by_lane["cargo-rail-cold"].runtime.available | not)
            or (.runtime.available | not)
            or $by_lane["cargo-rail-cold"].runtime.identity != .runtime.identity
          )
        )
        or (.repeated_sample_equivalence.output_manifest_identical | not)
        or (.repeated_sample_equivalence.runtime_output_identical | not)
      )
  ]
  | length;

def lane_minimum($samples):
  $samples
  | group_by([.workload, .lane])
  | map(select(any(.available)) | [.[] | select(.accepted and .available)] | length)
  | if length == 0 then 0 else min end;

def group_proof_complete:
  .group_validity.complete == true
  and .group_validity.output_manifests_identical == true
  and .group_validity.runtime_outputs_identical == true
  and .group_validity.action_census_identical == true
  and .group_validity.bypass_cache_io_zero == true
  and .group_validity.cache_event_streams_complete == true
  and .group_validity.cache_proof_replays_valid == true
  and .group_validity.cache_equivalent == true
  and .group_validity.cache_io_consistent == true
  and ((.group_validity.rejection_reasons // []) | length) == 0;

def accepted_measurement_group:
  length > 0
  and any(.[]; .available == true)
  and all(.[]; group_proof_complete and ((.available | not) or .accepted == true));

def one_lane($group; $lane):
  [$group[] | select(.lane == $lane)] as $matches
  | if ($matches | length) == 1 then $matches[0] else null end;

def paired_ratios($samples; $workload; $measured_lane; $reference_lane):
  [
    ($samples | group_by([.workload, .round])[])
    | select(.[0].workload == $workload and accepted_measurement_group)
    | . as $group
    | one_lane($group; $measured_lane) as $measured
    | one_lane($group; $reference_lane) as $reference
    | select(
        $measured != null
        and $reference != null
        and ($measured.resources.wall_seconds | finite_number)
        and ($reference.resources.wall_seconds | finite_number)
        and $reference.resources.wall_seconds > 0
      )
    | {
        round: $group[0].round,
        value: ($measured.resources.wall_seconds / $reference.resources.wall_seconds)
      }
  ]
  | sort_by(.round);

def paired_differences($samples; $workload; $measured_lane; $reference_lane):
  [
    ($samples | group_by([.workload, .round])[])
    | select(.[0].workload == $workload and accepted_measurement_group)
    | . as $group
    | one_lane($group; $measured_lane) as $measured
    | one_lane($group; $reference_lane) as $reference
    | select(
        $measured != null
        and $reference != null
        and ($measured.resources.wall_seconds | finite_number)
        and ($reference.resources.wall_seconds | finite_number)
      )
    | {
        round: $group[0].round,
        value: ($measured.resources.wall_seconds - $reference.resources.wall_seconds)
      }
  ]
  | sort_by(.round);

def paired_repayment($samples; $workload):
  [
    ($samples | group_by([.workload, .round])[])
    | select(.[0].workload == $workload and accepted_measurement_group)
    | . as $group
    | one_lane($group; "cargo-rail-cold") as $cold
    | one_lane($group; "native-cargo") as $native
    | one_lane($group; "cargo-rail-warm") as $warm
    | select(
        $cold != null
        and $native != null
        and $warm != null
        and ($cold.resources.wall_seconds | finite_number)
        and ($native.resources.wall_seconds | finite_number)
        and ($warm.resources.wall_seconds | finite_number)
        and $native.resources.wall_seconds > $warm.resources.wall_seconds
      )
    | {
        round: $group[0].round,
        value: (
          ([($cold.resources.wall_seconds - $native.resources.wall_seconds), 0] | max)
          / ($native.resources.wall_seconds - $warm.resources.wall_seconds)
        )
      }
  ]
  | sort_by(.round);

def paired_current_peer_ratios($samples; $workload):
  [
    ($samples | group_by([.workload, .round])[])
    | select(.[0].workload == $workload and accepted_measurement_group)
    | . as $group
    | one_lane($group; "cargo-rail-warm") as $warm
    | one_lane($group; "sccache") as $server
    | one_lane($group; "sccache-client-side") as $client_side
    | select(
        $warm != null
        and $server != null
        and $client_side != null
        and ($warm.resources.wall_seconds | finite_number)
        and ($server.resources.wall_seconds | finite_number)
        and ($client_side.resources.wall_seconds | finite_number)
        and $server.resources.wall_seconds > 0
        and $client_side.resources.wall_seconds > 0
      )
    | ([ $server.resources.wall_seconds, $client_side.resources.wall_seconds ] | min) as $peer
    | {
        round: $group[0].round,
        value: ($warm.resources.wall_seconds / $peer)
      }
  ]
  | sort_by(.round);

def pair_values($pairs):
  [$pairs[].value];

def paired_stats($pairs):
  pair_values($pairs) as $values
  | {
      sample_count: ($values | length),
      p50: distribution_free_bounds($values; 0.50; 0.95),
      p95: distribution_free_bounds($values; 0.95; 0.95)
    };

def workload_comparisons($samples; $workload):
  paired_differences($samples; $workload; "cargo-rail-delegated"; "native-cargo-warm") as $supervision
  | (paired_ratios($samples; $workload; "cargo-rail-cold"; "native-cargo") | map(.value -= 1)) as $cold
  | paired_repayment($samples; $workload) as $repayment
  | paired_ratios($samples; $workload; "cargo-rail-warm"; "native-cargo") as $l1_native
  | ($l1_native | map(.value = 1 - .value)) as $l1_improvement
  | paired_current_peer_ratios($samples; $workload) as $current_peer
  | ($current_peer | map(.value = 1 - .value)) as $peer_improvement
  | {
      supervision_overhead_seconds: paired_stats($supervision),
      cold_publication_overhead_fraction: paired_stats($cold),
      publication_repayment_reuses: paired_stats($repayment),
      l1_vs_native_normalized_time: paired_stats($l1_native),
      l1_vs_native_improvement_fraction: paired_stats($l1_improvement),
      l1_vs_current_sccache_normalized_time: paired_stats($current_peer),
      l1_vs_current_sccache_improvement_fraction: paired_stats($peer_improvement)
    };

def expected_group_keys($run):
  [
    $run.workloads[] as $workload
    | range(1; $run.attempts + 1) as $round
    | [$workload, $round]
  ]
  | sort;

def expected_lane_order($lanes; $round):
  ($lanes | length) as $lane_count
  | [range(0; $lane_count) as $position | $lanes[(($round - 1 + $position) % $lane_count)]];

def group_structure_valid($group; $run):
  ($group[0].round // null) as $round
  | ($group[0].workload // null) as $workload
  | ($group | sort_by(.interleave_position)) as $ordered
  | ($run.lanes | length) as $lane_count
  | ($group | length) == $lane_count
    and ($round | finite_number)
    and $round == ($round | floor)
    and ($run.workloads | index($workload)) != null
    and all($group[]; .round == $round and .workload == $workload)
    and ([$group[].lane] | sort) == ($run.lanes | sort)
    and ([$group[].interleave_position] | sort) == [range(0; $lane_count)]
    and ([$ordered[].lane] == expected_lane_order($run.lanes; $round))
    and ([$group[].accepted] | unique | length) == 1;

def structure_evidence($groups; $run):
  ([$groups[] | [.[0].workload, .[0].round]] | sort) as $observed_keys
  | expected_group_keys($run) as $expected_keys
  | {
      interleaving: $run.interleaving,
      observed_groups: ($groups | length),
      expected_groups: ($expected_keys | length),
      exact_group_keys: ($observed_keys == $expected_keys),
      malformed_groups: [
        $groups[]
        | select(group_structure_valid(.; $run) | not)
        | {workload: .[0].workload, round: .[0].round}
      ],
      passes: (
        $run.interleaving == "rotation-by-round"
        and $observed_keys == $expected_keys
        and all($groups[]; group_structure_valid(.; $run))
      )
    };

. as $samples
| $run[0] as $run_metadata
| ($samples | group_by([.workload, .round])) as $all_groups
| lane_minimum($samples) as $minimum
| false_hits($samples) as $false_hit_count
| structure_evidence($all_groups; $run_metadata) as $structure
| workload_comparisons($samples; "check") as $check_comparisons
| workload_comparisons($samples; "build") as $build_comparisons
| {
    schema_version: 14,
    run_id: $run_metadata.run_id,
    evidence_kind: $evidence_kind,
    runs: $runs,
    attempts: $attempts,
    warmup_runs: $warmup_runs,
    environment: $environment[0],
    cargo_rail_native_cache: $run_metadata.cargo_rail_native_cache,
    accepted_samples: ([$samples[] | select(.accepted and .available)] | length),
    rejected_samples: ([$samples[] | select((.accepted | not) and .available)] | length),
    workloads: {
      check: {
        native_cargo: lane_stats($samples; "check"; "native-cargo"),
        native_cargo_warm: lane_stats($samples; "check"; "native-cargo-warm"),
        native_cargo_incremental_warm: lane_stats($samples; "check"; "native-cargo-incremental-warm"),
        cargo_rail_delegated: lane_stats($samples; "check"; "cargo-rail-delegated"),
        cargo_rail_incremental_bypass: lane_stats($samples; "check"; "cargo-rail-incremental-bypass"),
        cargo_rail_disabled: lane_stats($samples; "check"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats($samples; "check"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats($samples; "check"; "cargo-rail-warm"),
        sccache_server: lane_stats($samples; "check"; "sccache"),
        sccache_client_side: lane_stats($samples; "check"; "sccache-client-side"),
        cargo_rail_sccache_preserved: lane_stats($samples; "check"; "cargo-rail-sccache-preserved")
      },
      build: {
        native_cargo: lane_stats($samples; "build"; "native-cargo"),
        native_cargo_warm: lane_stats($samples; "build"; "native-cargo-warm"),
        native_cargo_incremental_warm: lane_stats($samples; "build"; "native-cargo-incremental-warm"),
        cargo_rail_delegated: lane_stats($samples; "build"; "cargo-rail-delegated"),
        cargo_rail_incremental_bypass: lane_stats($samples; "build"; "cargo-rail-incremental-bypass"),
        cargo_rail_disabled: lane_stats($samples; "build"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats($samples; "build"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats($samples; "build"; "cargo-rail-warm"),
        sccache_server: lane_stats($samples; "build"; "sccache"),
        sccache_client_side: lane_stats($samples; "build"; "sccache-client-side"),
        cargo_rail_sccache_preserved: lane_stats($samples; "build"; "cargo-rail-sccache-preserved")
      }
    },
    comparisons: {
      check: $check_comparisons,
      build: $build_comparisons
    },
    validity: {
      finalized_groups: ($all_groups | length),
      expected_groups: $structure.expected_groups,
      accepted_groups: ([$all_groups[] | select(accepted_measurement_group)] | length),
      rejected_groups: ([$all_groups[] | select(any(.[]; .accepted == false))] | length),
      minimum_accepted_samples_per_lane: $minimum,
      interleaving: $structure,
      false_hits: $false_hit_count,
      measurement_complete: (
        $structure.passes
        and $minimum >= $runs
        and $false_hit_count == 0
      )
    },
    check_units: $check_units[0],
    build_units: $build_units[0],
    publication: {
      check: (
        if $check_seed[0] == null then null
          else $check_seed[0].status.local.cache
          + {repayment_reuses_p50: publication_repayment($samples; "check")}
        end
      ),
      build: (
        if $build_seed[0] == null then null
          else $build_seed[0].status.local.cache
          + {repayment_reuses_p50: publication_repayment($samples; "build")}
        end
      )
    },
    sccache: {
      available: $sccache_available,
      path: (if $sccache_path == "" then null else $sccache_path end),
      version: (if $sccache_version == "" then null else $sccache_version end),
      unavailable_reason: (if $sccache_unavailable_reason == "" then null else $sccache_unavailable_reason end),
      resource_accounting: $run_metadata.sccache.resource_accounting
    },
    native_check_cold: lane_stats($samples; "check"; "native-cargo"),
    cargo_rail_check_disabled: lane_stats($samples; "check"; "cargo-rail-disabled"),
    cargo_rail_check_cold: lane_stats($samples; "check"; "cargo-rail-cold"),
    cargo_rail_check_warm_same_root: lane_stats($samples; "check"; "cargo-rail-warm"),
    native_build_cold: lane_stats($samples; "build"; "native-cargo"),
    cargo_rail_build_disabled: lane_stats($samples; "build"; "cargo-rail-disabled"),
    cargo_rail_build_cold: lane_stats($samples; "build"; "cargo-rail-cold"),
    cargo_rail_build_warm_same_root: lane_stats($samples; "build"; "cargo-rail-warm")
  }
