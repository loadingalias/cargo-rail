"""Reject transferred cache evidence before executing mismatched or incomplete work."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

SPEC = importlib.util.spec_from_file_location('cache_host', Path(__file__).resolve().parents[1] / 'check-cache-host.py')
cache = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cache)


class CacheTransfer(unittest.TestCase):
    def test_driver_preparation_rejects_an_unsupported_target_without_publishing(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run(
                ['scripts/check-compiler-fact-driver.sh', '--prepare', temporary, 'unsupported-target'],
                cwd=cache.ROOT, capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('compiler driver cross preparation requires', result.stderr)
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_real_archive_runs_every_case_and_retains_failure_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'src').mkdir()
            (root / '.config').mkdir()
            config = (cache.ROOT / '.config/nextest.toml').read_text()
            (root / '.config/nextest.toml').write_text(
                config.splitlines()[0] + '\n[profile.cache-host]' + config.split('[profile.cache-host]', 1)[1] +
                '\n[[profile.cache-host.overrides]]\nfilter = "test(=a_failure)"\npriority = 100\n')
            shutil.copyfile(cache.ROOT / '.config/tooling.toml', root / '.config/tooling.toml')
            (root / '.gitignore').write_text('/target\n')
            (root / 'Cargo.toml').write_text('[package]\nname = "transfer-fixture"\nversion = "0.0.0"\nedition = "2024"\n')
            (root / 'src/lib.rs').write_text('''
#[test]
fn a_failure() {
    assert!(std::env::var_os("CACHE_TRANSFER_PROBE_FAIL").is_none(), "intentional transfer failure");
}
#[test]
fn b_sentinel() { eprintln!("remaining case executed"); }
''')
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(('CARGO_', 'NEXTEST_')) and key not in ('RUSTFLAGS', 'RUSTC', 'RUSTDOC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')}
            env['RUSTUP_TOOLCHAIN'] = subprocess.check_output(
                ['rustup', 'show', 'active-toolchain'], cwd=cache.ROOT, text=True).split()[0]
            env['CARGO_HOME'] = str(root / 'target/cargo-home')
            env['CARGO_RAIL_CACHE'] = 'off'
            env.pop('CACHE_TRANSFER_PROBE_FAIL', None)
            def run(*args):
                result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return result
            run('git', 'init', '--initial-branch=main')
            run('cargo', 'generate-lockfile', '--offline')
            run('git', 'add', '.')
            run('git', '-c', 'user.name=Transfer Test', '-c', 'user.email=transfer@example.invalid',
                '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', 'commit', '-m', 'fixture')
            directory = root / 'target/bundle'
            directory.mkdir(parents=True)
            cases = {'transfer_fixture': ['a_failure', 'b_sentinel']}
            with patch.dict(os.environ, env, clear=True), patch.object(cache, 'ROOT', root), \
                 patch.object(cache, 'cases_for', return_value=cases):
                compiler = cache.rustc_identity()
                run('cargo', 'nextest', 'archive', '--locked', '--target', compiler['host'],
                    '--archive-file', str(directory / 'tests.tar.zst'))
                import hashlib
                manifest = {'schema': 1, 'target': compiler['host'], 'source': cache.source_identity(),
                            'nextest': cache.nextest_identity(),
                            'rustc': {key: compiler[key] for key in ('release', 'commit-hash')}, 'cases': cases,
                            'archive_sha256': hashlib.sha256((directory / 'tests.tar.zst').read_bytes()).hexdigest()}
                (directory / 'manifest.json').write_text(json.dumps(manifest))
                for failed in (True, False):
                    before = set((root / 'target/cache-host-results').glob('run-*'))
                    with patch.dict(os.environ, {'CACHE_TRANSFER_PROBE_FAIL': '1'} if failed else {}), \
                         patch('sys.stdout', new=io.StringIO()):
                        if failed:
                            with self.assertRaises(subprocess.CalledProcessError):
                                cache.execute(directory)
                        else:
                            cache.execute(directory)
                    after = set((root / 'target/cache-host-results').glob('run-*'))
                    self.assertEqual(len(after - before), 1)
                    out = (after - before).pop()
                    summary = json.loads((out / 'summary.json').read_text())
                    self.assertEqual(summary['status'], 'failed' if failed else 'passed')
                    self.assertEqual(summary['source'], manifest['source'])
                    self.assertEqual(summary['exit_code'] == 0, not failed)
                    log = (out / 'nextest.log').read_text()
                    self.assertIn('remaining case executed', log)
                    self.assertEqual('intentional transfer failure' in log, failed)
                    if failed:
                        self.assertLess(log.index('intentional transfer failure'), log.index('remaining case executed'))
                    suites = ET.parse(out / 'junit.xml').getroot()
                    self.assertEqual(int(suites.attrib['tests']), 2)
                    self.assertEqual(int(suites.attrib['failures']), int(failed))

    def test_exact_required_tests_cannot_be_missing_ignored_or_supplemented(self):
        report = {'rust-suites': {'cargo-rail::cache': {'binary-name': 'cache', 'testcases': {
            'required': {'ignored': False, 'filter-match': {'status': 'matches'}},
            'excluded': {'ignored': False, 'filter-match': {'status': 'mismatch'}},
        }}}}
        cache.validate_nextest_cases(report, {'cache': ['required']})
        mutations = [
            lambda suite: suite['testcases'].pop('required'),
            lambda suite: suite['testcases']['required'].update(ignored=True),
            lambda suite: suite['testcases']['excluded'].update({'filter-match': {'status': 'matches'}}),
        ]
        for mutate in mutations:
            changed = copy.deepcopy(report)
            mutate(changed['rust-suites']['cargo-rail::cache'])
            with self.assertRaises(ValueError):
                cache.validate_nextest_cases(changed, {'cache': ['required']})
        duplicate = copy.deepcopy(report)
        duplicate['rust-suites']['duplicate'] = duplicate['rust-suites']['cargo-rail::cache']
        with self.assertRaises(ValueError):
            cache.validate_nextest_cases(duplicate, {'cache': ['required']})

    def test_archive_mismatch_fails_before_nextest_executes_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            archive = directory / 'tests.tar.zst'
            archive.write_bytes(b'archive contents')
            compiler = {'host': 'riscv64gc-unknown-linux-gnu', 'release': 'test-release', 'commit-hash': 'test-commit'}
            manifest = {'schema': 1, 'target': compiler['host'], 'source': {'commit': 'checkout', 'sha256': 'source'},
                        'nextest': ['pinned-nextest'], 'rustc': {'release': 'test-release', 'commit-hash': 'test-commit'},
                        'cases': cache.cases_for(compiler['host']),
                        'archive_sha256': ''}
            # The fixture digest is independently computed, not taken from the verifier.
            import hashlib
            manifest['archive_sha256'] = hashlib.sha256(b'archive contents').hexdigest()
            path = directory / 'manifest.json'
            path.write_text(json.dumps(manifest))
            with patch.object(cache, 'rustc_identity', return_value=compiler), \
                 patch.object(cache, 'nextest_identity', return_value=['pinned-nextest']), \
                 patch.object(cache, 'source_identity', return_value=manifest['source']), \
                 patch.object(cache.subprocess, 'run') as run:
                self.assertEqual(cache.verify_archive(directory), manifest)
                for field, invalid in [('schema', 2), ('target', 'x86_64-unknown-linux-gnu'),
                                       ('source', {}), ('nextest', []), ('rustc', {}), ('cases', {}),
                                       ('archive_sha256', '0' * 64)]:
                    with self.subTest(field=field), self.assertRaises(ValueError):
                        path.write_text(json.dumps({**manifest, field: invalid}))
                        cache.execute(directory)
                path.write_text(json.dumps(manifest))
                archive.write_bytes(b'corrupt archive')
                with self.assertRaises(ValueError):
                    cache.execute(directory)
                run.assert_not_called()

    def test_transfer_rejects_inherited_selectors_and_compiler_overrides(self):
        for name in ['NEXTEST_PROFILE', 'NEXTEST_TEST_THREADS', 'CARGO_PROFILE_DEV_OPT_LEVEL',
                     'CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_RUNNER', 'RUSTFLAGS',
                     'CARGO_RAIL_FACT_DRIVER_FILE']:
            with self.subTest(name=name), patch.dict(os.environ, {name: 'override'}, clear=True):
                with self.assertRaisesRegex(ValueError, name):
                    cache.transfer_environment()

    def test_linux_selection_keeps_the_linux_linker_contract(self):
        linux = cache.cases_for('riscv64gc-unknown-linux-gnu')
        darwin = cache.cases_for('aarch64-apple-darwin')
        self.assertEqual(sum(map(len, linux.values())), 21)
        self.assertEqual(sum(map(len, darwin.values())), 20)
        self.assertEqual(linux['cache'], darwin['cache'])
        self.assertEqual(linux['cargo_rail'][1:], darwin['cargo_rail'])


if __name__ == '__main__':
    unittest.main()
