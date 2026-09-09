#!/usr/bin/env python3
"""Run the native cache contract through Cargo-built standard Rust test harnesses."""
import json
from pathlib import Path
import subprocess
import sys

# Exact cases are shared with the full primary-platform suite. Missing or ignored
# cases are errors: platform gating must not silently reduce cache qualification.
CASES = {
    'cargo_rail': [
        'cache::cas::tests::native_manifest_must_match_the_validated_output_contract',
        'cache::cas::tests::malformed_native_action_state_is_durably_quarantined',
        'cache::cas::tests::concurrent_native_publications_converge_on_one_binding',
        'cache::cas::tests::native_restore_lock_serializes_across_processes',
        'cache::cas::tests::cache_open_unlinks_hostile_staging_links_without_following_them',
        'compiler::native_cache::tests::restore_commit_rejects_a_destination_created_after_authorization',
        'compiler::native_cache::tests::restore_recovery_discards_partial_private_records_before_authority',
        'compiler::native_cache::tests::every_pre_execution_mutation_changes_the_action_identity',
        'compiler::native_cache::tests::session_identity_changes_with_exact_compiler_authority',
        'compiler::native_cache::tests::native_search_restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation',
        'compiler::native_cache::tests::restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation',
        'remote_cache::object::tests::unique_entry_round_trips_one_compressed_pack',
        'remote_cache::object::tests::malformed_compressed_payload_is_rejected_as_integrity_failure',
    ],
    'integration': [
        'test_cache_installation::native_host_restores_and_executes_small_cargo_outputs',
        'test_cache_installation::direct_cargo_reuses_verified_outputs_and_off_never_touches_l1',
        'test_native_cache_fixture::native_assembly_inputs_bypass_after_proven_ordinary_cold_and_warm_reuse',
        'test_native_cache_fixture::transitive_crate_replacement_cannot_restore_a_result_for_stale_direct_metadata',
        'test_cache_installation::direct_s3_remote_is_l2_only_and_falls_back_cold_on_corruption_or_outage',
    ],
}

if sys.platform == 'linux':
    CASES['cargo_rail'].insert(
        0, 'compiler::native_cache::tests::gcc_driver_capture_preserves_startup_inputs_and_revalidates_selection',
    )


def validate_cases(binary, cases):
    listed = subprocess.check_output([binary, '--list', '--format=terse'], text=True)
    available = {line.removesuffix(': test') for line in listed.splitlines() if line.endswith(': test')}
    ignored = subprocess.check_output([binary, '--list', '--ignored', '--format=terse'], text=True)
    if not set(cases) <= available or any(f'{case}: test' in ignored.splitlines() for case in cases):
        raise ValueError(f'{binary}: required cache tests are missing or ignored')


def run(target_directory):
    built = subprocess.run(
        ['cargo', 'test', '--target-dir', target_directory, '--lib', '--test', 'integration',
         '--all-features', '--locked', '--no-run', '--message-format=json-render-diagnostics'],
        stdout=subprocess.PIPE, text=True, check=True,
    )
    binaries = {}
    for line in built.stdout.splitlines():
        message = json.loads(line)
        if message.get('reason') == 'compiler-artifact' and message['profile']['test'] and message.get('executable'):
            name = message['target']['name']
            if name in CASES:
                if name in binaries:
                    raise ValueError(f'ambiguous test executable: {name}')
                binaries[name] = message['executable']
    if set(binaries) != set(CASES):
        raise ValueError(f'missing cache qualification binaries: {set(CASES) - set(binaries)}')
    for name, cases in CASES.items():
        validate_cases(binaries[name], cases)
    for name, cases in CASES.items():
        for case in cases:
            print(f'Cache qualification: {case}', flush=True)
            subprocess.run([binaries[name], '--exact', case, '--test-threads=1', '--nocapture'], check=True)
    print(f'Native cache qualification passed: {sum(map(len, CASES.values()))} required cases.', flush=True)


if __name__ == '__main__':
    run(str(Path(sys.argv[1]).resolve()))
