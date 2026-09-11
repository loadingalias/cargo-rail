"""Exercise check recipes through their production shell entry points."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class CheckRecipes(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / 'scripts').mkdir()
        shutil.copy(ROOT / 'justfile', self.root)
        for name in ['check.sh', 'check-cross.sh', 'check-compiler-fact-driver.sh']:
            shutil.copy(ROOT / 'scripts' / name, self.root / 'scripts' / name)
        binaries = self.root / 'bin'
        binaries.mkdir()
        recorder = f'#!{sys.executable}\n' + '''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
if name == 'uname':
    print(os.environ['CHECK_HOST'])
else:
    with open(os.environ['CHECK_LOG'], 'a') as log:
        log.write(json.dumps({'args': [name, *sys.argv[1:]],
                              'rustdocflags': os.environ.get('RUSTDOCFLAGS'),
                              'bootstrap': os.environ.get('RUSTC_BOOTSTRAP')}) + '\\n')
    if sys.argv[1:2] == [os.environ.get('CHECK_FAIL_COMMAND')]:
        sys.exit(23)
'''
        for name in ['cargo', 'cargo-zigbuild', 'cargo-xwin', 'zig', 'uname']:
            binary = binaries / name
            binary.write_text(recorder)
            binary.chmod(0o755)
        self.environment = {
            'PATH': str(binaries) + os.pathsep + os.environ['PATH'],
            'HOME': str(self.root),
            'CHECK_LOG': str(self.root / 'commands.jsonl'),
            'CHECK_HOST': 'Darwin',
            'RUSTDOCFLAGS': '-C debuginfo=0',
        }

    def run_recipe(self, *recipes, success=True):
        log = Path(self.environment['CHECK_LOG'])
        log.unlink(missing_ok=True)
        result = subprocess.run(
            [shutil.which('just'), '--justfile', str(self.root / 'justfile'), *recipes],
            cwd=self.root, env=self.environment, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def test_shared_check_has_one_nonmutating_policy_on_every_host(self):
        driver = 'tools/compiler-fact-driver/Cargo.toml'
        for host in ['Darwin', 'Linux', 'MINGW64_NT']:
            with self.subTest(host=host):
                self.environment['CHECK_HOST'] = host
                calls = self.run_recipe('ci-check')
                self.assertEqual([call['args'] for call in calls], [
                    ['cargo', 'fmt', '--all', '--', '--check'],
                    ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features', '--locked'],
                    ['cargo', 'deny', '--locked', '--workspace', '--all-features', 'check', '-D', 'warnings', 'all'],
                    ['cargo', 'doc', '--workspace', '--no-deps', '--all-features', '--locked'],
                    ['cargo', 'fmt', '--manifest-path', driver, '--all', '--', '--check'],
                    ['cargo', 'clippy', '--manifest-path', driver, '--all-targets', '--all-features', '--locked', '--', '-D', 'warnings'],
                    ['cargo', 'test', '--manifest-path', driver, '--all-targets', '--all-features', '--locked'],
                ])
                self.assertEqual(calls[3]['rustdocflags'], '-C debuginfo=0 -D warnings')
                self.assertEqual([call['bootstrap'] for call in calls[4:]], ['cargo_rail_fact_driver'] * 3)

    def test_local_check_wraps_the_shared_lane_with_fixing_and_workstation_checks(self):
        shared = self.run_recipe('ci-check')
        calls = self.run_recipe('check')
        self.assertEqual([call['args'] for call in calls[:3]], [
            ['cargo', 'fmt', '--all'],
            ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features', '--locked', '--fix', '--allow-dirty', '--allow-staged'],
            ['cargo', 'fmt', '--all'],
        ])
        self.assertEqual(calls[3:10], shared)
        self.assertEqual([call['args'] for call in calls[10:]], [
            [*command, '--target', target, '--workspace', '--all-targets', '--all-features', '--locked']
            for command, target in [
                (['cargo-zigbuild', 'clippy'], 'x86_64-unknown-linux-gnu'),
                (['cargo-zigbuild', 'clippy'], 'aarch64-unknown-linux-gnu'),
                (['cargo-zigbuild', 'clippy'], 'x86_64-unknown-linux-musl'),
                (['cargo-zigbuild', 'clippy'], 'aarch64-unknown-linux-musl'),
                (['cargo', 'xwin', 'clippy'], 'x86_64-pc-windows-msvc'),
                (['cargo', 'xwin', 'clippy'], 'aarch64-pc-windows-msvc'),
            ]
        ] + [['cargo', 'rail', 'unify', '--check', '--explain']])

    def test_shared_check_failure_stops_later_checks(self):
        self.environment['CHECK_FAIL_COMMAND'] = 'clippy'
        calls = self.run_recipe('ci-check', success=False)
        self.assertEqual([call['args'][1] for call in calls], ['fmt', 'clippy'])

    def test_shared_failure_stops_local_cross_checks_and_dogfooding(self):
        self.environment['CHECK_FAIL_COMMAND'] = 'deny'
        calls = self.run_recipe('check', success=False)
        self.assertEqual([call['args'][1] for call in calls], ['fmt', 'clippy', 'fmt', 'fmt', 'clippy', 'deny'])


if __name__ == '__main__':
    unittest.main()
