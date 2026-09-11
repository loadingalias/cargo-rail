"""Reject transferred cache evidence before executing mismatched or incomplete work."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('cache_host', Path(__file__).resolve().parents[1] / 'check-cache-host.py')
cache = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cache)


class CacheTransfer(unittest.TestCase):
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
