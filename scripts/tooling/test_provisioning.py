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


class LinuxPerfProvisioning(unittest.TestCase):
    def provision(self, kernel, operation='ci', missing=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / 'scripts/tooling'
            scripts.mkdir(parents=True)
            for name in ('linux.sh', 'catalog.py'):
                shutil.copy(catalog.ROOT / 'scripts/tooling' / name, scripts / name)
            shutil.copytree(catalog.ROOT / '.config', root / '.config')
            binaries = root / 'bin'
            binaries.mkdir()
            recorder = f'#!{sys.executable}\n' + '''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
if name == 'uname':
    print({'-s': 'Linux', '-m': 'x86_64', '-r': os.environ['FIXTURE_KERNEL']}[sys.argv[1]])
elif name == 'id':
    print('0')
elif name == 'apt-cache':
    if not (os.environ['FIXTURE_MISSING'] == '1' and sys.argv[-1] == 'linux-tools-' + os.environ['FIXTURE_KERNEL']):
        print(sys.argv[-1] + ' | 1.2.3 | snapshot')
elif name == 'apt-get':
    with open(os.environ['FIXTURE_LOG'], 'a') as log:
        log.write(json.dumps(sys.argv[1:]) + '\\n')
    if '--allow-downgrades' in sys.argv:
        sys.exit(23)
'''
            for name in ('uname', 'id', 'apt-cache', 'apt-get'):
                path = binaries / name
                path.write_text(recorder)
                path.chmod(0o755)
            environment = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
                               FIXTURE_KERNEL=kernel, FIXTURE_MISSING='1' if missing else '0',
                               FIXTURE_LOG=str(root / 'commands.jsonl'))
            # Supply the Ubuntu OS boundary while running the actual installer on this host.
            command = '''
source() {
  if [[ "$1" == /etc/os-release ]]; then
    ID=ubuntu VERSION_ID=24.04 PRETTY_NAME=Ubuntu
  else
    builtin source "$@"
  fi
}
source "$1" x86_64-linux "$2"
'''
            result = subprocess.run(['bash', '-c', command, 'fixture', str(scripts / 'linux.sh'), operation],
                                    env=environment, text=True, capture_output=True)
            log = root / 'commands.jsonl'
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def test_perf_installs_the_running_kernel_flavor_from_the_snapshot(self):
        for kernel in ('6.8.0-1030-aws', '6.8.0-79-generic', '6.8.0-79-generic-64k'):
            with self.subTest(kernel=kernel):
                result, calls = self.provision(kernel)
                self.assertEqual(result.returncode, 23, result.stderr)
                install = calls[-1]
                self.assertIn('install', install)
                self.assertIn('linux-tools-common=1.2.3', install)
                self.assertIn(f'linux-tools-{kernel}=1.2.3', install)
                self.assertIn('Dir::Etc::sourceparts=-', install)

    def test_missing_kernel_tools_fails_before_installation(self):
        result, calls = self.provision('6.8.0-1030-aws', missing=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('missing Ubuntu package linux-tools-', result.stderr)
        self.assertIn(catalog.read()['linux']['snapshot'], result.stderr)
        self.assertIn('6.8.0-1030-aws', result.stderr)
        self.assertFalse(any('--allow-downgrades' in call for call in calls))

    def test_package_provisioning_does_not_require_kernel_tools(self):
        result, calls = self.provision('6.8.0-1030-aws', operation='package', missing=True)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertFalse(any(argument.startswith('linux-tools-') for argument in calls[-1]))


if __name__ == '__main__':
    unittest.main()
