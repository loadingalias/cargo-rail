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

# The pinned uv runs each helper as `python SCRIPT` would, in the interpreter that runs these tests.
UV_STUB = """import pathlib, runpy, sys
if sys.argv[1:3] == ['run', 'python']:
    sys.argv = sys.argv[3:]
    sys.path.insert(0, str(pathlib.Path(sys.argv[0]).parent))
    runpy.run_path(sys.argv[0], run_name='__main__')
"""


class UnixPackageProvisioning(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / 'scripts/tooling'
        scripts.mkdir(parents=True)
        for name in ['package-unix.sh', 'uv.sh', 'catalog.py']:
            shutil.copy(catalog.ROOT / 'scripts/tooling' / name, scripts / name)
        shutil.copytree(catalog.ROOT / '.config', self.root / '.config')
        for name in ['rust-toolchain.toml', '.python-version']:
            shutil.copy(catalog.ROOT / name, self.root)
        # The pinned uv is installed, so uv.sh finds it by its receipt.
        sha = catalog.read()['aarch64-macos']['assets']['uv']['sha256']
        self.uv_directory = self.root / '.local/share/cargo-rail-tooling/uv' / sha[:16]
        self.uv_directory.mkdir(parents=True)
        (self.uv_directory / '.rail-installed').write_text(sha + '\n')
        (self.uv_directory / 'uv').write_text(f'#!{sys.executable}\n' + UV_STUB)
        (self.uv_directory / 'uv').chmod(0o755)
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
            'GITHUB_PATH': str(self.root / 'github-path'),
        }

    def recording_command(self, path):
        path.write_text(self.recorder)
        path.chmod(0o755)

    def provision(self, target, operation=None, success=True):
        log = Path(self.environment['PROVISION_LOG'])
        log.unlink(missing_ok=True)
        arguments = [str(self.root / 'scripts/tooling/package-unix.sh'), target]
        if operation is not None:
            arguments.append(operation)
        result = subprocess.run(arguments,
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
            ['cargo', 'install', 'cargo-binstall', '--version', catalog.read()['versions']['cargo-binstall'], '--locked'],
            ['cargo', 'binstall', '--locked', '--no-confirm', '--targets', 'aarch64-apple-darwin',
             '--strategies', 'crate-meta-data', f"just@{catalog.read()['cargo']['just']}"],
        ])
        self.assertEqual((self.root / 'github-env').read_text(), f'RUSTUP_TOOLCHAIN={catalog.rust_channel()}\n')
        # Later steps run Python through the same pinned uv.
        self.assertEqual((self.root / 'github-path').read_text(), f'{self.uv_directory}\n')

    def test_macos_ci_operation_installs_the_cataloged_check_tools(self):
        calls = self.provision('aarch64-apple-darwin', 'ci')
        data = catalog.read()
        self.assertEqual(calls, [
            ['xcrun', '--find', 'clang'],
            ['cmake', '--version'],
            ['rustup', 'toolchain', 'install', catalog.rust_channel(), '--profile', 'minimal',
             '--component', 'clippy', '--component', 'rustfmt', '--component', 'rustc-dev',
             '--component', 'llvm-tools'],
            ['rustc', '-vV'],
            ['cargo', 'install', 'cargo-binstall', '--version', data['versions']['cargo-binstall'], '--locked'],
            *[
                ['cargo', 'binstall', '--locked', '--no-confirm', '--targets', 'aarch64-apple-darwin',
                 '--strategies', 'crate-meta-data', f"{tool}@{data['cargo'][tool]}"]
                for tool in ['cargo-deny', 'cargo-nextest', 'just']
            ],
        ])

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

    def test_unsupported_operation_fails_before_provisioning(self):
        self.assertEqual(self.provision('aarch64-apple-darwin', 'unknown', success=False), [])


LINUX_STUB = r"""import json, os, pathlib, runpy, sys, tomllib
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['FIXTURE_LOG'], 'a') as log:
    log.write(json.dumps([name, *args]) + '\n')
if name == 'uname':
    print({'-s': 'Linux', '-m': 'x86_64', '-r': os.environ['FIXTURE_KERNEL']}[args[0]])
elif name == 'id':
    print('0')
elif name == 'dpkg-query':
    # Only a fixture that says so already holds every requested package.
    if not os.environ.get('FIXTURE_PACKAGES_PRESENT'):
        sys.exit(1)
    for package in args[2:]:
        print('ii ' + package)
elif name == 'uv' and args[:2] == ['run', 'python']:
    script, *rest = args[2:]
    if pathlib.Path(script).name == 'catalog.py' and rest[0] == 'download':
        pathlib.Path(rest[-1]).write_text('#!/bin/sh\nprintf \'["rustup-init"]\\n\' >> "$FIXTURE_LOG"\n')
    elif pathlib.Path(script).name != 'verify.py':
        # Run the helper in this interpreter, as `python SCRIPT` would.
        sys.argv = [script, *rest]
        sys.path.insert(0, str(pathlib.Path(script).parent))
        runpy.run_path(script, run_name='__main__')
elif args[-1:] == ['--version']:
    # A converged host: each installed tool reports its catalog pin.
    tool = 'cargo-' + args[-2] if name == 'cargo' else name
    catalog = tomllib.loads(pathlib.Path(os.environ['FIXTURE_CATALOG']).read_text())
    pins = {**catalog['versions'], **catalog['cargo']}
    if not os.environ.get('FIXTURE_PINNED') or tool not in pins:
        sys.exit(1)
    print(f'{tool} {pins[tool]}')
"""


def package_installs(calls):
    """The package lists of APT installations, after the bootstrap of curl and the trust store."""
    installs = []
    for call in calls:
        if call[0] == 'apt-get' and 'install' in call:
            packages = [arg for arg in call[call.index('install') + 1:] if not arg.startswith('-')]
            if packages != ['curl', 'ca-certificates']:
                installs.append(packages)
    return installs


# A container shares its host's kernel, so the installer omits that kernel's perf tools.
CONTAINER = Path('/.dockerenv').exists() or Path('/run/.containerenv').exists()


class LinuxProvisioning(unittest.TestCase):
    """Run the real Linux installer against substitute host, package, and Rust commands."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        scripts = self.root / 'scripts/tooling'
        scripts.mkdir(parents=True)
        for name in ('linux.sh', 'uv.sh', 'x86_64-linux.sh', 'catalog.py', 'fingerprint.py', 'verify.py'):
            shutil.copy(catalog.ROOT / 'scripts/tooling' / name, scripts / name)
        shutil.copytree(catalog.ROOT / '.config', self.root / '.config')
        for name in ('rust-toolchain.toml', '.python-version'):
            shutil.copy(catalog.ROOT / name, self.root / name)
        binaries = self.root / 'bin'
        binaries.mkdir()
        for name in ('uname', 'id', 'apt-get', 'dpkg-query', 'rustup', 'rustc', 'cargo', 'just'):
            self.stub(binaries / name)
        (binaries / 'bash').symlink_to(shutil.which('bash'))
        # Pinned archives, including uv, are installed: each is found by its receipt.
        self.home = self.root / 'home'
        prefix = self.home / '.local/share/cargo-rail-tooling'
        for name, asset in catalog.read()['x86_64-linux']['assets'].items():
            if name != 'rustup':
                directory = prefix / name / asset['sha256'][:16]
                directory.mkdir(parents=True)
                (directory / '.rail-installed').write_text(asset['sha256'] + '\n')
        self.uv_directory = prefix / 'uv' / catalog.read()['x86_64-linux']['assets']['uv']['sha256'][:16]
        self.stub(self.uv_directory / 'uv')
        (self.root / 'temporary').mkdir()
        bash_environment = self.root / 'bash-environment'
        bash_environment.write_text("""source() {
  if [[ "$1" == /etc/os-release ]]; then
    ID=ubuntu VERSION_ID=24.04 PRETTY_NAME=fixture
  else
    builtin source "$@"
  fi
}
""")
        self.environment = {
            'PATH': f'{binaries}:/usr/bin:/bin', 'HOME': str(self.home), 'TMPDIR': str(self.root / 'temporary'),
            'BASH_ENV': str(bash_environment), 'FIXTURE_LOG': str(self.root / 'commands.jsonl'),
            'FIXTURE_CATALOG': str(self.root / '.config/tooling.toml'), 'FIXTURE_KERNEL': '6.8.0-1030-aws',
            'GITHUB_PATH': str(self.root / 'github-path'), 'GITHUB_ENV': str(self.root / 'github-env'),
        }

    def stub(self, path):
        path.write_text(f'#!{sys.executable}\n' + LINUX_STUB)
        path.chmod(0o755)

    def provision(self, *arguments, success=True, **environment):
        log = Path(self.environment['FIXTURE_LOG'])
        log.unlink(missing_ok=True)
        result = subprocess.run([str(self.root / 'bin/bash'), str(self.root / 'scripts/tooling/linux.sh'),
                                 'x86_64-linux', *arguments],
                                env={**self.environment, **environment}, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        return result, calls

    def fingerprint(self, operation):
        return subprocess.check_output(
            [sys.executable, str(self.root / 'scripts/tooling/fingerprint.py'), 'x86_64-linux', operation], text=True)

    def test_ci_installs_guest_packages_and_publishes_the_pinned_uv(self):
        _, calls = self.provision('ci')
        selected = catalog.selection(catalog.read(), 'x86_64-linux', 'ci')
        kernel_tools = [] if CONTAINER else ['linux-tools-6.8.0-1030-aws']
        self.assertEqual(package_installs(calls), [selected['packages'] + kernel_tools])
        for call in (call for call in calls if call[0] == 'apt-get'):
            self.assertIn('Acquire::Retries=3', call)
            self.assertFalse(any(argument.startswith('Dir::') for argument in call), call)
            self.assertNotIn('--allow-downgrades', call)
        self.assertIn(['uv', 'python', 'install'], calls)
        self.assertIn(['rustup-init'], calls)
        installs = [call[-3] for call in calls if call[:1] == ['cargo'] and 'install' in call]
        self.assertEqual(installs, selected['cargo'])
        self.assertEqual((self.root / 'github-path').read_text().splitlines()[0], str(self.uv_directory))
        environment = (self.home / '.local/share/cargo-rail-tooling/environment.sh').read_text()
        self.assertIn(str(self.uv_directory), environment)
        self.assertEqual((self.home / '.local/share/cargo-rail-tooling/fingerprint.ci').read_text(), self.fingerprint('ci'))

    @unittest.skipIf(CONTAINER, 'a container omits the kernel tools')
    def test_perf_installs_the_running_kernel_flavor(self):
        for kernel in ('6.8.0-1030-aws', '6.8.0-79-generic', '6.8.0-79-generic-64k'):
            with self.subTest(kernel=kernel):
                _, calls = self.provision('ci', FIXTURE_KERNEL=kernel)
                [packages] = package_installs(calls)
                self.assertIn('linux-tools-common', packages)
                self.assertEqual(packages[-1], f'linux-tools-{kernel}')

    def test_package_provisioning_does_not_require_kernel_tools(self):
        _, calls = self.provision('package')
        [packages] = package_installs(calls)
        self.assertEqual(packages, catalog.selection(catalog.read(), 'x86_64-linux', 'package')['packages'])

    def test_installed_packages_skip_the_archive(self):
        result, calls = self.provision('ci', FIXTURE_PACKAGES_PRESENT='1')
        self.assertIn('Ubuntu packages are installed.', result.stdout)
        self.assertEqual(package_installs(calls), [])

    def test_converged_tools_are_kept_and_the_run_records_its_fingerprint(self):
        _, calls = self.provision('ci', FIXTURE_PINNED='1')
        self.assertNotIn(['rustup-init'], calls)
        self.assertFalse(any(call[0] == 'cargo' and ('install' in call or 'binstall' in call) for call in calls))
        self.assertTrue(any(call[:3] == ['rustup', 'toolchain', 'install'] for call in calls))
        self.assertEqual((self.home / '.local/share/cargo-rail-tooling/fingerprint.ci').read_text(), self.fingerprint('ci'))

    def test_a_matching_fingerprint_republishes_without_installing(self):
        self.provision('ci', FIXTURE_PINNED='1')
        (self.root / 'github-path').unlink()
        result, calls = self.provision('ci', FIXTURE_PINNED='1')
        self.assertIn(f"Tooling fingerprint {self.fingerprint('ci').strip()} matches", result.stdout)
        self.assertEqual(package_installs(calls), [])
        self.assertFalse(any(call[0] in ('cargo', 'rustup') and ('install' in call or 'binstall' in call) for call in calls))
        self.assertFalse(any(Path(call[3]).name == 'verify.py' for call in calls if call[:3] == ['uv', 'run', 'python']))
        self.assertEqual((self.root / 'github-path').read_text().splitlines()[0], str(self.uv_directory))
        # Another operation has its own fingerprint, so it still converges.
        result, _ = self.provision('package', FIXTURE_PINNED='1')
        self.assertNotIn('matches', result.stdout)

    def test_a_matching_fingerprint_with_a_missing_tool_converges(self):
        self.provision('ci', FIXTURE_PINNED='1')
        result, calls = self.provision('ci')
        self.assertIn('a pinned tool is missing; converging', result.stderr)
        installs = [call[-3] for call in calls if call[:1] == ['cargo'] and 'install' in call]
        self.assertEqual(installs, catalog.selection(catalog.read(), 'x86_64-linux', 'ci')['cargo'])

    def test_cross_build_always_converges(self):
        self.provision('cross-build', 'riscv64-linux', FIXTURE_PINNED='1')
        _, calls = self.provision('cross-build', 'riscv64-linux', FIXTURE_PINNED='1')
        self.assertTrue(any(call[:3] == ['rustup', 'toolchain', 'install'] for call in calls))
        self.assertFalse(any(Path(call[3]).name == 'fingerprint.py' for call in calls if call[:3] == ['uv', 'run', 'python']))
        self.assertEqual(list((self.home / '.local/share/cargo-rail-tooling').glob('fingerprint.*')), [])

    def test_unknown_operation_fails_before_provisioning(self):
        result, calls = self.provision('typo', success=False)
        self.assertEqual(result.returncode, 64)
        self.assertEqual(calls, [])


if __name__ == '__main__':
    unittest.main()
