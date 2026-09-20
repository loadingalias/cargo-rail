"""Exercise the release packager against Cargo's installable binary inventory."""
import copy
import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('package_release', ROOT / 'scripts/package-release.py')
if SPEC is None or SPEC.loader is None:
    raise ImportError('cannot load the release packager')
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class ReleasePackage(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        metadata = json.loads(subprocess.check_output(
            ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline'], cwd=ROOT,
        ))
        cls.package = next(item for item in metadata['packages'] if item['name'] == 'cargo-rail')

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.components = self.root / 'target/release'
        self.components.mkdir(parents=True)
        self.destination = self.root / 'assets'
        self.version = self.package['version']
        (self.root / 'Cargo.toml').write_text(f'[package]\nversion = "{self.version}"\n')
        (self.root / 'LICENSE').write_bytes(b'MIT license fixture\n')
        self.packages = [copy.deepcopy(self.package)]
        self.packages[0]['manifest_path'] = str(self.root / 'Cargo.toml')
        self.metadata = {'target_directory': str(self.root / 'target'), 'packages': self.packages}

    def prepare(self, target):
        self.target = target
        self.suffix = '.exe' if target.endswith('windows-msvc') else ''
        # Independent release component contract, including files Cargo does not install.
        self.names = {
            name + self.suffix for name in [
                'cargo-rail', 'cargo-rail-compiler-observation', 'cargo-rail-native-rustc-wrapper',
                'cargo-rail-native-rustc-worker', 'cargo-rail-distributed-worker', 'cargo-rail-fact-driver',
            ]
        } | {'cargo-rail-fact-driver-source-v1.json'}
        for name in self.names:
            (self.components / name).write_bytes(f'fixture: {name}\n'.encode())
        authority = {
            'CARGO_RAIL_FACT_DRIVER_RUSTC_HOST': target,
            'CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE': 'fixture-release',
            'CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT': 'fixture-commit',
        }
        for prefix, name in [
            ('CARGO_RAIL_FACT_DRIVER', 'cargo-rail-fact-driver' + self.suffix),
            ('CARGO_RAIL_FACT_DRIVER_SOURCE', 'cargo-rail-fact-driver-source-v1.json'),
        ]:
            authority[prefix + '_FILE'] = name
            authority[prefix + '_SHA256'] = 'sha256:' + hashlib.sha256((self.components / name).read_bytes()).hexdigest()
        (self.components / 'compiler-driver-authority.env').write_text(''.join(
            f'export {key}={value}\n' for key, value in authority.items()
        ))

    def command_output(self, args, **kwargs):
        if args == ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline']:
            return json.dumps(self.metadata).encode()
        if args == ['rustc', '-vV']:
            return f'host: {self.target}\nrelease: fixture-release\ncommit-hash: fixture-commit\n'
        if args == [str(self.components / ('cargo-rail' + self.suffix)), 'rail', '--version']:
            return f'cargo-rail {self.version}\n'
        self.fail(f'unexpected subprocess: {args}')

    def package_release(self):
        with patch.object(PACKAGER, '__file__', str(self.root / 'scripts/package-release.py')), \
                patch.object(PACKAGER.subprocess, 'check_output', side_effect=self.command_output):
            PACKAGER.package(self.destination)

    def test_archive_contains_default_install_binaries_and_authenticated_components(self):
        benchmark = next(item for item in self.package['targets'] if item['name'] == 'cargo-rail-bench')
        self.assertEqual(benchmark['required-features'], ['bench'])
        self.assertEqual(self.package['features']['bench'], [])
        self.assertNotIn('bench', self.package['features'].get('default', []))
        for target in ['aarch64-apple-darwin', 'x86_64-pc-windows-msvc']:
            with self.subTest(target=target):
                self.destination = self.root / target
                self.prepare(target)
                self.package_release()
                archive_path = self.destination / f'cargo-rail-{target}.zip'
                with zipfile.ZipFile(archive_path) as archive:
                    expected = self.names | {'LICENSE', 'cargo-rail-components-v1.tsv'}
                    self.assertEqual(set(archive.namelist()), {f'cargo-rail/{name}' for name in expected})
                    for item in self.package['targets']:
                        if 'bin' in item['kind'] and not item.get('required-features'):
                            self.assertIn(f'cargo-rail/{item["name"]}{self.suffix}', archive.namelist())
                    manifest = archive.read('cargo-rail/cargo-rail-components-v1.tsv').decode().splitlines()
                    self.assertEqual(manifest[0], f'cargo-rail-components-v1\t{self.version}\t{target}')
                    self.assertEqual({line.split('\t')[0] for line in manifest[1:]}, self.names | {'LICENSE'})
                    for line in manifest[1:]:
                        name, digest, size, _ = line.split('\t')
                        content = archive.read(f'cargo-rail/{name}')
                        self.assertEqual(hashlib.sha256(content).hexdigest(), digest)
                        self.assertEqual(len(content), int(size))
                self.assertEqual(
                    (self.destination / 'SHA256SUMS').read_bytes(),
                    f'{hashlib.sha256(archive_path.read_bytes()).hexdigest()}  {archive_path.name}\n'.encode(),
                )

    def test_mandatory_binary_omission_rejects_packaging_before_output(self):
        self.prepare('aarch64-apple-darwin')
        benchmark = next(item for item in self.packages[0]['targets'] if item['name'] == 'cargo-rail-bench')
        benchmark.pop('required-features')
        with self.assertRaisesRegex(ValueError, 'release archive omits mandatory Cargo binaries: cargo-rail-bench'):
            self.package_release()
        self.assertFalse(self.destination.exists())

    def test_missing_built_binary_rejects_packaging_before_output(self):
        self.prepare('x86_64-pc-windows-msvc')
        (self.components / 'cargo-rail-native-rustc-worker.exe').unlink()
        with self.assertRaisesRegex(ValueError, 'component is not a bounded regular file: cargo-rail-native-rustc-worker.exe'):
            self.package_release()
        self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
