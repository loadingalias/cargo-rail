#!/usr/bin/env python3
"""Run the native cache contract through Cargo-built standard Rust test harnesses."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import tomllib
import sys

# Exact cases are shared with the full primary-platform suite. Missing or ignored
# cases are errors: platform gating must not silently reduce cache qualification.
CASES = {
    'cargo_rail': [
        'compiler::native_cache::tests::linker_capture_outlives_source_discovery_without_losing_content_validation',
        'compiler::native_cache::tests::linker_capture_enforces_file_path_and_content_work_bounds',
        'cache::cas::tests::native_manifest_must_match_the_validated_output_contract',
        'cache::cas::tests::malformed_native_action_state_is_durably_quarantined',
        'cache::cas::tests::concurrent_native_publications_converge_on_one_binding',
        'cache::cas::tests::native_restore_lock_serializes_across_processes',
        'cache::cas::tests::cache_open_unlinks_hostile_staging_links_without_following_them',
        'compiler::native_cache::tests::restore_commit_rejects_a_destination_created_after_authorization',
        'compiler::native_cache::tests::restore_recovery_discards_partial_private_records_before_authority',
        'compiler::native_cache::tests::session_identity_changes_with_exact_compiler_authority',
        'compiler::native_cache::tests::native_search_restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation',
        'compiler::native_cache::tests::restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation',
        'remote_cache::object::tests::unique_entry_round_trips_one_compressed_pack',
        'remote_cache::object::tests::malformed_compressed_payload_is_rejected_as_integrity_failure',
    ],
    'cache': [
        'test_cache_installation::native_host_restores_and_executes_small_cargo_outputs',
        'test_cache_installation::direct_cargo_reuses_verified_outputs_and_off_never_touches_l1',
        'test_native_cache_fixture::native_assembly_inputs_bypass_after_proven_ordinary_cold_and_warm_reuse',
        'test_native_cache_fixture::transitive_crate_replacement_cannot_restore_a_result_for_stale_direct_metadata',
        'test_cache_installation::direct_s3_remote_is_l2_only_and_falls_back_cold_on_corruption_or_outage',
        'test_distributed_compilation::mutual_tls_worker_executes_through_machine_owned_cargo_setup',
    ],
}

ROOT = Path(__file__).resolve().parents[1]


def cases_for(target):
    cases = {name: list(tests) for name, tests in CASES.items()}
    if '-linux-' in target:
        cases['cargo_rail'].insert(
            0, 'compiler::native_cache::tests::gcc_driver_capture_preserves_startup_inputs_and_revalidates_selection',
        )
    return cases


def validate_cases(binary, cases):
    listed = subprocess.check_output([binary, '--list', '--format=terse'], text=True)
    available = {line.removesuffix(': test') for line in listed.splitlines() if line.endswith(': test')}
    ignored = subprocess.check_output([binary, '--list', '--ignored', '--format=terse'], text=True)
    if not set(cases) <= available or any(f'{case}: test' in ignored.splitlines() for case in cases):
        raise ValueError(f'{binary}: required cache tests are missing or ignored')


def run(target_directory):
    built = subprocess.run(
        ['cargo', 'test', '--profile', 'cache-host', '--target-dir', target_directory, '--lib', '--test', 'cache',
         '--all-features', '--locked', '--no-run', '--message-format=json-render-diagnostics'],
        stdout=subprocess.PIPE, text=True, check=True,
    )
    cases = cases_for(rustc_identity()["host"])
    binaries = {}
    environment = dict(os.environ, CARGO_MANIFEST_DIR=str(ROOT))
    for line in built.stdout.splitlines():
        message = json.loads(line)
        if message.get('reason') == 'compiler-artifact' and message.get('executable') and message['target']['kind'] == ['bin']:
            environment['CARGO_BIN_EXE_' + message['target']['name']] = message['executable']
        if message.get('reason') == 'compiler-artifact' and message['profile']['test'] and message.get('executable'):
            name = message['target']['name']
            if name in cases:
                if name in binaries:
                    raise ValueError(f'ambiguous test executable: {name}')
                binaries[name] = message['executable']
    if set(binaries) != set(cases):
        raise ValueError(f'missing cache qualification binaries: {set(cases) - set(binaries)}')
    for name, required in cases.items():
        validate_cases(binaries[name], required)
    for name, required in cases.items():
        for case in required:
            print(f'Cache qualification: {case}', flush=True)
            subprocess.run([binaries[name], '--exact', case, '--test-threads=1', '--nocapture'], env=environment, check=True)
    print(f'Native cache qualification passed: {sum(map(len, cases.values()))} required cases.', flush=True)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def source_identity():
    names = subprocess.check_output(
        ['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=ROOT,
    ).decode().split('\0')
    rows = []
    for name in sorted(set(names) - {''}):
        path = ROOT / name
        if path.is_symlink():
            raise ValueError(f'source symlink is unsupported: {name}')
        rows.append([name, digest(path) if path.is_file() else None,
                     bool(path.stat().st_mode & 0o111) if path.exists() else False])
    return {
        'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'sha256': hashlib.sha256(json.dumps(rows, separators=(',', ':')).encode()).hexdigest(),
    }


def rustc_identity(env=None):
    report = subprocess.check_output(['rustc', '-vV'], text=True, env=env)
    return dict(line.split(': ', 1) for line in report.splitlines() if ': ' in line)


def nextest_identity():
    report = subprocess.check_output(['cargo', 'nextest', '--version'], text=True)
    pin = tomllib.loads((ROOT / '.config/tooling.toml').read_text())['cargo']['cargo-nextest']
    if report.splitlines()[0].split()[:2] != ['cargo-nextest', pin]:
        raise ValueError(f'install the pinned cargo-nextest {pin} before transferring tests')
    # Host differs by design; the source commit and release must match exactly.
    return [line for line in report.splitlines() if not line.startswith('host: ')]


def transfer_environment():
    forbidden = {'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC', 'RUSTDOC', 'CARGO_BUILD_TARGET',
                 'CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                 'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTFLAGS'}
    for name, value in os.environ.items():
        if value and (name in forbidden or name.startswith(('NEXTEST_', 'CARGO_PROFILE_', 'CARGO_TARGET_',
                                                           'CARGO_RAIL_FACT_DRIVER_'))):
            raise ValueError(f'{name} overrides cache archive qualification; unset it')
    return dict(os.environ)


def prepare(target, directory):
    env = transfer_environment()
    host = rustc_identity(env)['host']
    if target == 'riscv64gc-unknown-linux-gnu':
        if host != 'x86_64-unknown-linux-gnu':
            raise ValueError('RISC-V archives must be prepared on native x86-64 Linux')
        channel = tomllib.loads((ROOT / '.config/tooling.toml').read_text())['riscv64-linux']['rust-channel']
        env['RUSTUP_TOOLCHAIN'] = channel
        env.update(CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER='riscv64-linux-gnu-gcc',
                   CC_riscv64gc_unknown_linux_gnu='riscv64-linux-gnu-gcc',
                   CXX_riscv64gc_unknown_linux_gnu='riscv64-linux-gnu-g++',
                   AR_riscv64gc_unknown_linux_gnu='riscv64-linux-gnu-ar')
        if shutil.which('riscv64-linux-gnu-gcc') is None:
            raise ValueError('install the riscv-build tooling before preparing RISC-V tests')
    elif target != host:
        raise ValueError('cache archives support the native host or the RISC-V cross toolchain')
    version = nextest_identity()
    identity = source_identity()
    compiler = rustc_identity(env)
    if directory.exists():
        raise ValueError(f'refusing to overwrite existing cache evidence: {directory}')
    directory.parent.mkdir(parents=True, exist_ok=True)
    build = ROOT / 'target/cache-transfer-build'
    components = build / target / 'cache-host'
    subprocess.run(['scripts/check-compiler-fact-driver.sh', '--prepare-source', str(components)],
                   cwd=ROOT, env=env, check=True)
    for line in (components / 'compiler-driver-authority.env').read_text().splitlines():
        export, assignment = shlex.split(line)
        name, value = assignment.split('=', 1)
        if export != 'export' or name not in {'CARGO_RAIL_FACT_DRIVER_SOURCE_FILE',
                                             'CARGO_RAIL_FACT_DRIVER_SOURCE_SHA256',
                                             'CARGO_RAIL_FACT_DRIVER_SOURCE_PROVENANCE'}:
            raise ValueError('source preparation emitted unexpected compiler authority')
        env[name] = value
    source_name = env['CARGO_RAIL_FACT_DRIVER_SOURCE_FILE']
    (components / 'deps').mkdir(exist_ok=True)
    shutil.copyfile(components / source_name, components / 'deps' / source_name)
    # Library harnesses and Cargo binaries discover authenticated components beside themselves.
    include = [{'path': f'{target}/cache-host/{prefix}{source_name}', 'relative-to': 'target', 'on-missing': 'error'}
               for prefix in ('', 'deps/')]
    with tempfile.TemporaryDirectory(prefix='.cache-transfer-', dir=directory.parent) as temporary:
        config = Path(temporary) / 'nextest.toml'
        pending = Path(temporary) / 'bundle'
        pending.mkdir()
        config.write_text((ROOT / '.config/nextest.toml').read_text() +
                          '\n[profile.cache-host.archive]\ninclude = ' +
                          '[' + ', '.join('{ ' + ', '.join(f'{key} = {json.dumps(value)}' for key, value in item.items()) + ' }'
                                          for item in include) + ']\n')
        subprocess.run(['cargo', 'nextest', 'archive', '--config-file', str(config), '--profile', 'cache-host',
                        '--cargo-profile', 'cache-host', '--target', target, '--target-dir', str(build), '--lib', '--test', 'cache',
                        '--all-features', '--locked', '--archive-file', str(pending / 'tests.tar.zst')],
                       cwd=ROOT, env=env, check=True)
        if source_identity() != identity:
            raise ValueError('source changed during archive preparation; prepare again')
        manifest = {'schema': 1, 'target': target, 'source': identity, 'nextest': version,
                    'rustc': {key: compiler[key] for key in ('release', 'commit-hash')},
                    'cases': cases_for(target), 'archive_sha256': digest(pending / 'tests.tar.zst')}
        (pending / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        pending.rename(directory)
    print(f'Prepared cache tests for {target}: {directory}', flush=True)


def verify_archive(directory):
    if {path.name for path in directory.iterdir()} != {'manifest.json', 'tests.tar.zst'}:
        raise ValueError('cache transfer requires exactly its manifest and test archive')
    if any(path.is_symlink() or not path.is_file() for path in directory.iterdir()):
        raise ValueError('cache transfer inputs must be regular files')
    manifest = json.loads((directory / 'manifest.json').read_text())
    compiler = rustc_identity()
    expected = {'schema': 1, 'target': compiler['host'], 'source': source_identity(),
                'nextest': nextest_identity(), 'rustc': {key: compiler[key] for key in ('release', 'commit-hash')},
                'cases': cases_for(compiler['host']), 'archive_sha256': digest(directory / 'tests.tar.zst')}
    if manifest != expected:
        raise ValueError('cache archive source, target, toolchain, selection, or checksum mismatch')
    return manifest


def validate_nextest_cases(report, cases):
    selected = {}
    for suite in report['rust-suites'].values():
        for name, test in suite['testcases'].items():
            if test['filter-match']['status'] == 'matches':
                if test['ignored']:
                    raise ValueError(f'required cache test is ignored: {name}')
                selected.setdefault(suite['binary-name'], []).append(name)
    if {name: sorted(tests) for name, tests in selected.items()} != {name: sorted(tests) for name, tests in cases.items()}:
        raise ValueError('required cache tests are missing, duplicated, or supplemented')


def execute(directory):
    env = transfer_environment()
    env['CARGO_BUILD_JOBS'] = env.get('CARGO_BUILD_JOBS', '2')
    manifest = verify_archive(directory)
    cases = manifest['cases']
    filters = ' | '.join(f'(binary(={binary}) & test(={case}))'
                         for binary, tests in cases.items() for case in tests)
    with tempfile.TemporaryDirectory(prefix='cargo-rail-cache-run-') as temporary:
        args = ['--workspace-remap', str(ROOT), '--config-file', str(ROOT / '.config/nextest.toml'),
                '--profile', 'cache-host', '-E', filters]
        report = json.loads(subprocess.check_output(['cargo', 'nextest', 'list', *args, '--archive-file', str(directory / 'tests.tar.zst'),
                                                      '--extract-to', temporary, '--message-format', 'json'],
                                                   cwd=ROOT, env=env, text=True))
        validate_nextest_cases(report, cases)
        target = Path(temporary) / 'target'
        subprocess.run(['cargo', 'nextest', 'run', *args,
                        '--cargo-metadata', str(target / 'nextest/cargo-metadata.json'),
                        '--binaries-metadata', str(target / 'nextest/binaries-metadata.json'),
                        '--target-dir-remap', str(target), '--no-tests', 'fail'],
                       cwd=ROOT, env=env, check=True)
    verify_archive(directory)
    print(f'Native cache qualification passed: {sum(map(len, cases.values()))} required cases.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='operation', required=True)
    native = commands.add_parser('native')
    native.add_argument('directory')
    archive = commands.add_parser('prepare')
    archive.add_argument('target')
    archive.add_argument('directory', type=Path)
    execute_parser = commands.add_parser('run')
    execute_parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    if args.operation == 'native':
        run(args.directory)
    elif args.operation == 'prepare':
        prepare(args.target, args.directory.resolve())
    else:
        execute(args.directory.resolve())
