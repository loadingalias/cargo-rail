"""Run the native cache contract through Cargo-built standard Rust test harnesses."""
import argparse
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import tomllib

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
# Runners without release archives execute suites that x86-64 Linux builds: tooling platform and GNU prefix.
CROSS_TARGETS = {
    'riscv64gc-unknown-linux-gnu': ('riscv64-linux', 'riscv64-linux-gnu'),
    's390x-unknown-linux-gnu': ('s390x-linux', 's390x-linux-gnu'),
    'powerpc64le-unknown-linux-gnu': ('powerpc64le-linux', 'powerpc64le-linux-gnu'),
}
# Test harnesses find these components by the paths `just test` exports; the archive carries them.
SOURCE_INSTALLATION = 'source-installation-test'


def cases_for(target):
    if '-windows-' in target:
        # The required cases prove native result reuse, which Windows does not implement yet.
        raise ValueError('native cache qualification is unavailable on Windows: native result reuse is not implemented '
                         '(docs/caching.md#native-host-eligibility)')
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


def cross_environment(target):
    """The build environment for `target`: its runner's toolchain channel and GNU cross tools on x86-64 Linux."""
    env = transfer_environment()
    host = rustc_identity(env)['host']
    if target in CROSS_TARGETS:
        if host != 'x86_64-unknown-linux-gnu':
            raise ValueError('cross-built test archives must be prepared on native x86-64 Linux')
        platform, prefix = CROSS_TARGETS[target]
        env['RUSTUP_TOOLCHAIN'] = (tomllib.loads((ROOT / '.config/tooling.toml').read_text())[platform].get('rust-channel')
                                   or tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel'])
        normalized = target.replace('-', '_')
        env.update({f'CARGO_TARGET_{normalized.upper()}_LINKER': f'{prefix}-gcc', f'CC_{normalized}': f'{prefix}-gcc',
                    f'CXX_{normalized}': f'{prefix}-g++', f'AR_{normalized}': f'{prefix}-ar'})
        if shutil.which(f'{prefix}-gcc') is None:
            raise ValueError(f'install the x86_64-linux cross-build {platform} tooling before preparing its tests')
    elif target != host:
        raise ValueError('test archives support the native host or a cross-built runner target')
    return env


def check(target):
    """Run the workspace Clippy and documentation checks for `target` without executing its code."""
    env = cross_environment(target)
    for features in (['--all-features'], []):
        subprocess.run(['cargo', 'clippy', '--target', target, '--workspace', '--all-targets', *features, '--locked'],
                       cwd=ROOT, env=env, check=True)
    docs = dict(env, RUSTDOCFLAGS=' '.join(filter(None, (env.get('RUSTDOCFLAGS'), '-D warnings'))))
    subprocess.run(['cargo', 'doc', '--target', target, '--workspace', '--no-deps', '--all-features', '--locked'],
                   cwd=ROOT, env=docs, check=True)


def prepare(target, directory):
    env = cross_environment(target)
    version = nextest_identity()
    identity = source_identity()
    compiler = rustc_identity(env)
    if directory.exists():
        raise ValueError(f'refusing to overwrite existing cache evidence: {directory}')
    directory.parent.mkdir(parents=True, exist_ok=True)
    build = ROOT / 'target/cache-transfer-build'
    components = build / target / 'cache-host'
    subprocess.run(['scripts/check-compiler-fact-driver.sh', '--prepare', str(components), target],
                   cwd=ROOT, env=dict(env, CARGO_TARGET_DIR=str(build / 'compiler-driver')), check=True)
    authority = {}
    for line in (components / 'compiler-driver-authority.env').read_text().splitlines():
        export, assignment = shlex.split(line)
        name, value = assignment.split('=', 1)
        if export != 'export' or name in authority:
            raise ValueError('component preparation emitted invalid compiler authority')
        authority[name] = value
    required = {'CARGO_RAIL_FACT_DRIVER_' + field for field in (
        'FILE', 'SHA256', 'PROVENANCE', 'RUSTC_RELEASE', 'RUSTC_COMMIT', 'RUSTC_HOST',
        'COMPILER_LIBRARY', 'COMPILER_LIBRARY_SHA256', 'SOURCE_FILE', 'SOURCE_SHA256', 'SOURCE_PROVENANCE',
    )}
    if set(authority) != required | {'CARGO_RAIL_TEST_FACT_DRIVER', 'CARGO_RAIL_TEST_COMPONENT_BINARY'}:
        raise ValueError('component preparation requires complete compiler and source authority')
    if authority['CARGO_RAIL_FACT_DRIVER_RUSTC_HOST'] != target:
        raise ValueError('prepared compiler driver does not match the archive target')
    # The component-free CLI that source-installation tests run; `check-source-installation.sh` builds it natively.
    subprocess.run(['cargo', 'build', '--bin', 'cargo-rail', '--all-features', '--locked', '--profile', 'cache-host',
                    '--target', target, '--target-dir', str(build / SOURCE_INSTALLATION)], cwd=ROOT, env=env, check=True)
    env.update({name: authority[name] for name in required})
    names = [authority['CARGO_RAIL_FACT_DRIVER_FILE'], authority['CARGO_RAIL_FACT_DRIVER_SOURCE_FILE']]
    (components / 'deps').mkdir(exist_ok=True)
    for name in names:
        shutil.copy2(components / name, components / 'deps' / name)
    # Library harnesses and Cargo binaries discover authenticated components beside themselves.
    include = [{'path': f'{target}/cache-host/{prefix}{name}', 'relative-to': 'target', 'on-missing': 'error'}
               for prefix in ('', 'deps/') for name in names]
    include.append({'path': f'{SOURCE_INSTALLATION}/{target}/cache-host/cargo-rail', 'relative-to': 'target',
                    'on-missing': 'error'})
    with tempfile.TemporaryDirectory(prefix='.cache-transfer-', dir=directory.parent) as temporary:
        config = Path(temporary) / 'nextest.toml'
        pending = Path(temporary) / 'bundle'
        pending.mkdir()
        config.write_text((ROOT / '.config/nextest.toml').read_text() +
                          '\n[profile.cache-host.archive]\ninclude = ' +
                          '[' + ', '.join('{ ' + ', '.join(f'{key} = {json.dumps(value)}' for key, value in item.items()) + ' }'
                                          for item in include) + ']\n')
        subprocess.run(['cargo', 'nextest', 'archive', '--config-file', str(config), '--profile', 'cache-host',
                        '--cargo-profile', 'cache-host', '--target', target, '--target-dir', str(build), '--workspace',
                        '--all-features', '--locked', '--archive-file', str(pending / 'tests.tar.zst')],
                       cwd=ROOT, env=env, check=True)
        if source_identity() != identity:
            raise ValueError('source changed during archive preparation; prepare again')
        manifest = {'schema': 2, 'target': target, 'source': identity, 'nextest': version,
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
    expected = {'schema': 2, 'target': compiler['host'], 'source': source_identity(),
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


def execute(directory, suite=False):
    """Run the required qualification cases, or with `suite` the complete workspace suite, from an archive."""
    env = transfer_environment()
    env['CARGO_BUILD_JOBS'] = env.get('CARGO_BUILD_JOBS', '2')
    manifest = verify_archive(directory)
    cases = manifest['cases']
    profile = 'slow-host' if suite else 'cache-host'
    results = ROOT / ('target/suite-results' if suite else 'target/cache-host-results')
    results.mkdir(parents=True, exist_ok=True)
    out = Path(tempfile.mkdtemp(prefix='run-', dir=results))
    (out / 'extracted').mkdir()
    config = out / 'nextest.toml'
    config.write_text((ROOT / '.config/nextest.toml').read_text() +
                      '\n[store]\ndir = ' + json.dumps(str(out / 'store')) + '\n')
    shutil.copyfile(directory / 'manifest.json', out / 'manifest.json')
    started = time.monotonic()
    summary = {'status': 'failed', 'source': manifest['source'], 'target': manifest['target'],
               'selection': 'workspace' if suite else cases, 'nextest': manifest['nextest'], 'rustc': manifest['rustc']}
    junit = out / f'store/{profile}/junit.xml'
    print(f'{"Test suite" if suite else "Native cache qualification"} evidence: {out}', flush=True)
    try:
        args = ['--workspace-remap', str(ROOT), '--config-file', str(config), '--profile', profile]
        if not suite:
            args += ['-E', ' | '.join(f'(binary(={binary}) & test(={case}))'
                                      for binary, tests in cases.items() for case in tests)]
        listing = subprocess.run(['cargo', 'nextest', 'list', *args, '--archive-file', str(directory / 'tests.tar.zst'),
                                  '--extract-to', str(out / 'extracted'), '--message-format', 'json'],
                                 cwd=ROOT, env=env, text=True, capture_output=True, check=False)
        (out / 'list.log').write_text(listing.stderr)
        (out / 'tests.json').write_text(listing.stdout)
        print(listing.stderr, end='', flush=True)
        listing.check_returncode()
        if not suite:
            validate_nextest_cases(json.loads(listing.stdout), cases)
        target = out / 'extracted/target'
        # The paths `just test` exports after preparing these components, inside the extracted archive.
        components = target / manifest['target'] / 'cache-host'
        env.update(CARGO_RAIL_TEST_FACT_DRIVER=str(components / 'cargo-rail-fact-driver'),
                   CARGO_RAIL_TEST_COMPONENT_BINARY=str(components / 'cargo-rail'),
                   CARGO_RAIL_TEST_SOURCE_BINARY=str(target / SOURCE_INSTALLATION / manifest['target'] / 'cache-host/cargo-rail'))
        command = ['cargo', 'nextest', 'run', *args,
                   '--cargo-metadata', str(target / 'nextest/cargo-metadata.json'),
                   '--binaries-metadata', str(target / 'nextest/binaries-metadata.json'),
                   '--target-dir-remap', str(target), '--no-tests', 'fail']
        if not suite:
            command.append('--no-capture')
        with (out / 'nextest.log').open('w') as log, subprocess.Popen(
            command, cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        ) as process:
            assert process.stdout is not None
            for line in process.stdout:
                print(line, end='', flush=True)
                log.write(line)
                log.flush()
            summary['exit_code'] = process.wait()
        verify_archive(directory)
        if summary['exit_code']:
            raise subprocess.CalledProcessError(summary['exit_code'], command)
        if not suite and not junit.is_file():
            raise ValueError('native cache qualification did not produce its JUnit report')
        summary['status'] = 'passed'
    finally:
        summary['elapsed_seconds'] = time.monotonic() - started
        (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        if junit.is_file():
            shutil.copyfile(junit, out / 'junit.xml')
    if suite:
        print('Transferred test suite passed.', flush=True)
    else:
        print(f'Native cache qualification passed: {sum(map(len, cases.values()))} required cases.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='operation', required=True)
    native = commands.add_parser('native')
    native.add_argument('directory')
    check_parser = commands.add_parser('check')
    check_parser.add_argument('target')
    archive = commands.add_parser('prepare')
    archive.add_argument('target')
    archive.add_argument('directory', type=Path)
    execute_parser = commands.add_parser('run')
    execute_parser.add_argument('directory', type=Path)
    suite_parser = commands.add_parser('run-suite')
    suite_parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    if args.operation == 'native':
        run(args.directory)
    elif args.operation == 'check':
        check(args.target)
    elif args.operation == 'prepare':
        prepare(args.target, args.directory.resolve())
    elif args.operation == 'run-suite':
        execute(args.directory.resolve(), suite=True)
    else:
        execute(args.directory.resolve())
