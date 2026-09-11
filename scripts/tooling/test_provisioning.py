"""Check Unix package provisioning without changing the host toolchain."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import catalog


class UnixPackageProvisioning(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / 'scripts/tooling'
        scripts.mkdir(parents=True)
        for name in ['package-unix.sh', 'catalog.py']:
            shutil.copy(catalog.ROOT / 'scripts/tooling' / name, scripts / name)
        shutil.copytree(catalog.ROOT / '.config', self.root / '.config')
        shutil.copy(catalog.ROOT / 'rust-toolchain.toml', self.root)
        self.recorder = f'#!{sys.executable}\n' + '''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
if name == 'uname':
    print('Darwin' if sys.argv[1] == '-s' else 'arm64')
else:
    with open(os.environ['PROVISION_LOG'], 'a') as log:
        log.write(json.dumps([name, *sys.argv[1:]]) + '\\n')
    if name == os.environ.get('PROVISION_FAIL'):
        sys.exit(23)
    if name == 'rustc':
        print('host: aarch64-apple-darwin')
'''
        binaries = self.root / 'bin'
        binaries.mkdir()
        for name in ['uname', 'rustup', 'cargo', 'rustc', 'xcrun', 'cc', 'cmake']:
            self.recording_command(binaries / name)
        self.environment = {
            'PATH': str(binaries) + os.pathsep + os.environ['PATH'],
            'HOME': str(self.root),
            'PROVISION_LOG': str(self.root / 'commands.jsonl'),
            'GITHUB_ENV': str(self.root / 'github-env'),
        }

    def recording_command(self, path):
        path.write_text(self.recorder)
        path.chmod(0o755)

    def provision(self, target, success=True):
        log = Path(self.environment['PROVISION_LOG'])
        log.unlink(missing_ok=True)
        result = subprocess.run([str(self.root / 'scripts/tooling/package-unix.sh'), target],
                                env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def test_macos_uses_package_tools_and_exports_the_selected_toolchain(self):
        calls = self.provision('aarch64-apple-darwin')
        self.assertEqual(calls, [
            ['xcrun', '--find', 'clang'],
            ['cmake', '--version'],
            ['rustup', 'toolchain', 'install', catalog.rust_channel(), '--profile', 'minimal',
             '--component', 'rustc-dev', '--component', 'llvm-tools'],
            ['rustc', '-vV'],
            ['cargo', 'install', 'just', '--version', catalog.read()['cargo']['just'], '--locked'],
        ])
        self.assertEqual((self.root / 'github-env').read_text(), f'RUSTUP_TOOLCHAIN={catalog.rust_channel()}\n')

    def test_linux_dispatches_the_package_selection_to_existing_installers(self):
        for target, installer in [('x86_64-unknown-linux-gnu', 'x86_64-linux.sh'),
                                  ('aarch64-unknown-linux-gnu', 'aarch64-linux.sh')]:
            with self.subTest(target=target):
                self.recording_command(self.root / 'scripts/tooling' / installer)
                self.assertEqual(self.provision(target), [[installer, 'package']])

    def test_toolchain_failure_prevents_cargo_install_and_environment_publication(self):
        self.environment['PROVISION_FAIL'] = 'rustup'
        calls = self.provision('aarch64-apple-darwin', success=False)
        self.assertEqual([call[0] for call in calls], ['xcrun', 'cmake', 'rustup'])
        self.assertFalse((self.root / 'github-env').exists())

    def test_unsupported_target_fails_before_provisioning(self):
        self.assertEqual(self.provision('unknown-target', success=False), [])


if __name__ == '__main__':
    unittest.main()
