def percentile($values; $fraction):
  ($values | sort) as $sorted
  | if ($sorted | length) == 0 then null
    else $sorted[(((($sorted | length) * $fraction) | ceil) - 1)]
    end;

def lane_stats($workload; $lane):
  [.[] | select(.accepted and .available and .workload == $workload and .lane == $lane)] as $samples
  | [$samples[] | select(.resources.accounting.cpu_time.complete)] as $cpu_samples
  | [$samples[] | select(.resources.accounting.peak_rss.complete)] as $rss_samples
  | {
      accepted_samples: ($samples | length),
      rejected_samples: ([.[] | select(.available and (.accepted | not) and .workload == $workload and .lane == $lane)] | length),
      p50_seconds: percentile([$samples[].resources.wall_seconds]; 0.50),
      p95_seconds: percentile([$samples[].resources.wall_seconds]; 0.95),
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
        wall_time_complete: ($samples | length > 0 and all(.resources.accounting.wall_time.complete)),
        cpu_time_complete: ($samples | length > 0 and all(.resources.accounting.cpu_time.complete)),
        peak_rss_complete: ($samples | length > 0 and all(.resources.accounting.peak_rss.complete)),
        cpu_time_scope: ($samples | map(.resources.accounting.cpu_time.scope) | unique),
        cpu_time_unavailable_reasons: (
          $samples | map(.resources.accounting.cpu_time.reason // empty) | unique
        ),
        peak_rss_scope: ($samples | map(.resources.accounting.peak_rss.scope) | unique),
        peak_rss_unavailable_reasons: (
          $samples | map(.resources.accounting.peak_rss.reason // empty) | unique
        )
      },
      cache: {
        setup_bytes_hashed_p50: percentile([$samples[].cache.setup_bytes_hashed]; 0.50),
        setup_bytes_hashed_p95: percentile([$samples[].cache.setup_bytes_hashed]; 0.95),
        unit_bytes_hashed_p50: percentile([$samples[].cache.unit_bytes_hashed]; 0.50),
        unit_bytes_hashed_p95: percentile([$samples[].cache.unit_bytes_hashed]; 0.95),
        cache_bytes_read_p50: percentile([$samples[].cache.cache_bytes_read]; 0.50),
        cache_bytes_read_p95: percentile([$samples[].cache.cache_bytes_read]; 0.95),
        cache_bytes_written_p50: percentile([$samples[].cache.cache_bytes_written]; 0.50),
        cache_bytes_written_p95: percentile([$samples[].cache.cache_bytes_written]; 0.95),
        bytes_restored_p50: percentile([$samples[].cache.bytes_restored]; 0.50),
        bytes_restored_p95: percentile([$samples[].cache.bytes_restored]; 0.95)
      },
      acquisitions: {
        cargo_metadata_loads_p50: percentile([$samples[].execution.diagnostics.cargo_metadata_loads]; 0.50),
        cargo_metadata_loads_p95: percentile([$samples[].execution.diagnostics.cargo_metadata_loads]; 0.95),
        target_view_loads_p50: percentile([$samples[].execution.diagnostics.target_view_loads]; 0.50),
        target_view_loads_p95: percentile([$samples[].execution.diagnostics.target_view_loads]; 0.95),
        git_subprocesses_p50: percentile([$samples[].execution.diagnostics.git_subprocesses]; 0.50),
        git_subprocesses_p95: percentile([$samples[].execution.diagnostics.git_subprocesses]; 0.95),
        compiler_processes_p50: percentile([$samples[].action_census.count]; 0.50),
        compiler_processes_p95: percentile([$samples[].action_census.count]; 0.95)
      }
    };

def publication_repayment($workload):
  lane_stats($workload; "native-cargo") as $native
  | lane_stats($workload; "cargo-rail-cold") as $cold
  | lane_stats($workload; "cargo-rail-warm") as $warm
  | if $native.p50_seconds == null or $cold.p50_seconds == null or $warm.p50_seconds == null
      or $native.p50_seconds <= $warm.p50_seconds
    then null
    else ([($cold.p50_seconds - $native.p50_seconds), 0] | max)
      / ($native.p50_seconds - $warm.p50_seconds)
    end;

def overhead_budget($workload; $lane; $baseline; $p50_budget; $p95_budget):
  lane_stats($workload; $lane) as $measured
  | lane_stats($workload; $baseline) as $reference
  | if $measured.p50_seconds == null or $measured.p95_seconds == null
      or $reference.p50_seconds == null or $reference.p95_seconds == null
    then {
      available: false,
      lane: $lane,
      baseline: $baseline,
      p50_budget_seconds: $p50_budget,
      p95_budget_seconds: $p95_budget
    }
    else {
      available: true,
      lane: $lane,
      baseline: $baseline,
      p50_overhead_seconds: ($measured.p50_seconds - $reference.p50_seconds),
      p95_overhead_seconds: ($measured.p95_seconds - $reference.p95_seconds),
      p50_budget_seconds: $p50_budget,
      p95_budget_seconds: $p95_budget,
      passes: (
        ($measured.p50_seconds - $reference.p50_seconds) <= $p50_budget
        and ($measured.p95_seconds - $reference.p95_seconds) <= $p95_budget
      )
    }
    end;

def false_hits:
  [
    group_by([.workload, .round])[] as $group
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

def lane_minimum:
  group_by([.workload, .lane])
  | map(select(any(.available)) | [.[] | select(.accepted and .available)] | length)
  | if length == 0 then 0 else min end;

group_by([.workload, .round]) as $groups
| lane_minimum as $minimum
| {
    schema_version: 11,
    evidence_kind: $evidence_kind,
    publishable: (
      $evidence_kind == "retained"
      and ($groups | length) == ($attempts * 2)
      and $minimum >= $runs
      and false_hits == 0
    ),
    runs: $runs,
    attempts: $attempts,
    warmup_runs: $warmup_runs,
    environment: $environment[0],
    cargo_rail_native_cache: $run[0].cargo_rail_native_cache,
    accepted_samples: ([.[] | select(.accepted and .available)] | length),
    rejected_samples: ([.[] | select((.accepted | not) and .available)] | length),
    workloads: {
      check: {
        native_cargo: lane_stats("check"; "native-cargo"),
        native_cargo_warm: lane_stats("check"; "native-cargo-warm"),
        native_cargo_incremental_warm: lane_stats("check"; "native-cargo-incremental-warm"),
        cargo_rail_delegated: lane_stats("check"; "cargo-rail-delegated"),
        cargo_rail_incremental_bypass: lane_stats("check"; "cargo-rail-incremental-bypass"),
        cargo_rail_disabled: lane_stats("check"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats("check"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats("check"; "cargo-rail-warm"),
        sccache_0_16: lane_stats("check"; "sccache"),
        cargo_rail_sccache_preserved: lane_stats("check"; "cargo-rail-sccache-preserved")
      },
      build: {
        native_cargo: lane_stats("build"; "native-cargo"),
        native_cargo_warm: lane_stats("build"; "native-cargo-warm"),
        native_cargo_incremental_warm: lane_stats("build"; "native-cargo-incremental-warm"),
        cargo_rail_delegated: lane_stats("build"; "cargo-rail-delegated"),
        cargo_rail_incremental_bypass: lane_stats("build"; "cargo-rail-incremental-bypass"),
        cargo_rail_disabled: lane_stats("build"; "cargo-rail-disabled"),
        cargo_rail_cold: lane_stats("build"; "cargo-rail-cold"),
        cargo_rail_warm: lane_stats("build"; "cargo-rail-warm"),
        sccache_0_16: lane_stats("build"; "sccache"),
        cargo_rail_sccache_preserved: lane_stats("build"; "cargo-rail-sccache-preserved")
      }
    },
    budgets: {
      check: {
        no_op_supervision: overhead_budget("check"; "cargo-rail-delegated"; "native-cargo-warm"; 0.007; 0.012),
        static_bypass: overhead_budget(
          "check";
          "cargo-rail-incremental-bypass";
          "native-cargo-incremental-warm";
          0.050;
          0.100
        ),
        wrapper_preserved: overhead_budget(
          "check";
          "cargo-rail-sccache-preserved";
          "sccache";
          0.050;
          0.100
        )
      },
      build: {
        no_op_supervision: overhead_budget("build"; "cargo-rail-delegated"; "native-cargo-warm"; 0.007; 0.012),
        static_bypass: overhead_budget(
          "build";
          "cargo-rail-incremental-bypass";
          "native-cargo-incremental-warm";
          0.050;
          0.100
        ),
        wrapper_preserved: overhead_budget(
          "build";
          "cargo-rail-sccache-preserved";
          "sccache";
          0.050;
          0.100
        )
      }
    },
    validity: {
      finalized_groups: ($groups | length),
      expected_groups: ($attempts * 2),
      accepted_groups: ([$groups[] | select(length == 10 and all(.accepted))] | length),
      rejected_groups: ([$groups[] | select(any(.accepted | not))] | length),
      minimum_accepted_samples_per_lane: $minimum,
      false_hits: false_hits,
      measurement_complete: (($groups | length) == ($attempts * 2) and $minimum >= $runs and false_hits == 0)
    },
    check_units: $check_units[0],
    build_units: $build_units[0],
    publication: {
      check: (
        if $check_seed[0] == null then null
        else $check_seed[0].status.local.cache + {repayment_reuses_p50: publication_repayment("check")}
        end
      ),
      build: (
        if $build_seed[0] == null then null
        else $build_seed[0].status.local.cache + {repayment_reuses_p50: publication_repayment("build")}
        end
      )
    },
    sccache_0_16: {
      available: $sccache_available,
      path: (if $sccache_path == "" then null else $sccache_path end),
      version: (if $sccache_version == "" then null else $sccache_version end),
      unavailable_reason: (if $sccache_unavailable_reason == "" then null else $sccache_unavailable_reason end),
      resource_accounting: $run[0].sccache.resource_accounting
    },
    native_check_cold: lane_stats("check"; "native-cargo"),
    cargo_rail_check_disabled: lane_stats("check"; "cargo-rail-disabled"),
    cargo_rail_check_cold: lane_stats("check"; "cargo-rail-cold"),
    cargo_rail_check_warm_cross_root: lane_stats("check"; "cargo-rail-warm"),
    native_build_cold: lane_stats("build"; "native-cargo"),
    cargo_rail_build_disabled: lane_stats("build"; "cargo-rail-disabled"),
    cargo_rail_build_cold: lane_stats("build"; "cargo-rail-cold"),
    cargo_rail_build_warm_cross_root: lane_stats("build"; "cargo-rail-warm")
  }
