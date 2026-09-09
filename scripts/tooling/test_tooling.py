"""Behavior checks for provisioning and stable release updates."""
import copy
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

import tomlkit
import catalog
import update
import verify


class CatalogCommand(unittest.TestCase):
    def test_empty_tool_inventory_emits_no_shell_array_element(self):
        output = subprocess.run(
            [sys.executable, str(Path(catalog.__file__).resolve()), 'get', 'riscv64-linux', 'cargo'],
            check=True, capture_output=True,
        )
        self.assertEqual(output.stdout, b'')

    def test_runner_channel_selects_pinned_override_and_default(self):
        for platform, expected in [('riscv64-linux', catalog.read()['riscv64-linux']['rust-channel']),
                                   ('x86_64-linux', catalog.rust_channel()),
                                   ('x86_64-win', catalog.rust_channel())]:
            output = subprocess.check_output(
                [sys.executable, str(Path(catalog.__file__).resolve()), 'rust-channel', platform], text=True)
            self.assertEqual(output.strip(), expected)


class CompilerSelection(unittest.TestCase):
    def test_native_compiler_and_cargo_must_match_the_exact_pin(self):
        platform = 'riscv64-linux'
        channel = catalog.rust_channel(platform)
        rustc = 'rustc 1.98.0-nightly\nhost: riscv64gc-unknown-linux-gnu\ncommit-hash: expected\n'
        cargo = 'cargo 1.98.0-nightly\ncommit-hash: expected\n'
        for changed in (None, 'rustc', 'cargo', 'host'):
            def run(*arguments):
                if arguments[0] == 'rustup':
                    self.assertEqual(arguments[:3], ('rustup', 'run', channel))
                    return rustc if arguments[3] == 'rustc' else cargo
                value = rustc if arguments[0] == 'rustc' else cargo
                if arguments[0] == changed:
                    value = value.replace('expected', 'different')
                if changed == 'host' and arguments[0] == 'rustc':
                    value = value.replace('riscv64gc', 'x86_64')
                return value
            with self.subTest(changed=changed), patch.object(verify, 'run', side_effect=run):
                if changed is None:
                    verify.verify_rust(platform)
                else:
                    with self.assertRaises(ValueError):
                        verify.verify_rust(platform)


class ReleaseSelection(unittest.TestCase):
    def test_latest_stable_release_respects_rust_floor_and_yanks_without_age_filter(self):
        versions = [{'num': '1.0.0'}, {'num': '2.0.0', 'rust_version': '1.98.1'},
                    {'num': '3.0.0', 'yanked': True}, {'num': '4.0.0-rc.1'},
                    {'num': '5.0.0', 'rust_version': '1.99.0'}]
        self.assertEqual(update.eligible_release(versions, rust_version='1.98.1'), '2.0.0')
        self.assertEqual(update.eligible_release(versions), '5.0.0')
        with self.assertRaises(ValueError):
            update.eligible_release([{'num': '1.0.0', 'yanked': True}])

    def test_build_metadata_is_stable_and_does_not_enter_cargo_requirements(self):
        versions = [{'num': '0.23.9'}, {'num': '0.25.13+spec-1.1.0'},
                    {'num': '0.26.0-rc.1+spec-1.1.0'}]
        self.assertEqual(update.eligible_release(versions), '0.25.13')

    def test_excluded_path_dependency_resolves_through_its_fixture_consumer(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            sources = {
                'Cargo.toml': '[workspace]\n[package]\nname="container"\nversion="0.1.0"\n',
                'fixture/Cargo.toml': '[workspace]\nexclude=["dependency"]\n[package]\nname="consumer"\nversion="0.1.0"\n[dependencies]\nleaf={path="dependency"}\n',
                'fixture/dependency/Cargo.toml': '[package]\nname="leaf"\nversion="0.1.0"\n',
            }
            for name, source in sources.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
                (path.parent / 'src').mkdir(exist_ok=True)
                (path.parent / 'src/lib.rs').write_text('')
            documents = {root / name: tomlkit.parse(source) for name, source in sources.items()}
            with patch.object(update, 'ROOT', root):
                update.update_locks(documents)
            self.assertTrue((root / 'fixture/Cargo.lock').is_file())
            self.assertFalse((root / 'fixture/dependency/Cargo.lock').exists())

    def test_all_manifests_preserve_comments_aliases_and_local_dependencies(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            first = root / 'Cargo.toml'
            first.write_text('[workspace]\nmembers=[]\n[workspace.dependencies]\nserde="1"\n')
            second = root / 'tools/driver/Cargo.toml'
            second.parent.mkdir(parents=True)
            second.write_text('''# Keep me
[workspace]
[package]
name="driver"
version="0.1.0"
rust-version="1.98.1"
[dependencies]
local={path="../local",version="0.1.0"}
inherited.workspace=true
[target.'cfg(unix)'.build-dependencies]
aliased={package="serde",version="=1",features=["derive"]} # keep
''')
            with patch.object(update, 'ROOT', root), patch.object(update, 'crate_version', return_value='2.0.0'):
                documents = update.manifest_documents()
                self.assertEqual(set(documents), {first, second})
                update.update_manifests(documents)
            result = tomlkit.parse(second.read_text())
            self.assertEqual(result['dependencies']['local']['version'], '0.1.0')
            self.assertTrue(result['dependencies']['inherited']['workspace'])
            self.assertEqual(result['target']['cfg(unix)']['build-dependencies']['aliased']['version'], '=2.0.0')
            self.assertEqual(result['target']['cfg(unix)']['build-dependencies']['aliased']['features'], ['derive'])
            self.assertIn('# Keep me', second.read_text())
            self.assertIn('# keep', second.read_text())
            self.assertEqual(tomlkit.parse(first.read_text())['workspace']['dependencies']['serde'], '2.0.0')

    def test_lookup_failure_leaves_manifests_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = [root / 'first.toml', root / 'second.toml']
            originals = ['[dependencies]\na="1"\n', '[dependencies]\nb="1"\n']
            for path, source in zip(paths, originals): path.write_text(source)
            documents = {path: tomlkit.parse(path.read_text()) for path in paths}
            with patch.object(update, 'crate_version', side_effect=['2.0.0', ValueError('lookup failed')]):
                with self.assertRaises(ValueError): update.update_manifests(documents)
            self.assertEqual([path.read_text() for path in paths], originals)

    def test_locks_use_plain_cargo_deduplicate_roots_and_skip_fixture_templates(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            manifest = root / 'Cargo.toml'
            member = root / 'member/Cargo.toml'
            template = root / 'fixture/Cargo.toml'
            for path, source in ((manifest, '[workspace]'), (member, '[package]'),
                                 (template, '# __FIXTURE_GIT_URL__')):
                path.parent.mkdir(exist_ok=True)
                path.write_text(source)
            with patch.object(update, 'ROOT', root), patch.object(update, 'run', return_value=str(manifest)), patch.object(update.subprocess, 'run') as run:
                update.update_locks({manifest: {}, member: {}, template: {}})
                run.assert_called_once_with(['cargo', 'update', '--manifest-path', str(manifest)], cwd=root, check=True)


class CatalogPolicy(unittest.TestCase):
    def test_rust_override_requires_a_dated_nightly(self):
        for channel in ('nightly', 'stable', 'nightly-latest'):
            data = catalog.read()
            data['riscv64-linux']['rust-channel'] = channel
            with self.assertRaisesRegex(ValueError, 'dated nightly'):
                catalog.validate(data)

    def test_native_profiles_are_complete_and_cannot_inherit_full_tooling(self):
        data = catalog.read()
        catalog.validate(data)
        for name in ('riscv64-linux', 's390x-linux', 'powerpc64le-linux'):
            for field, value in [('cargo', ['cargo-nextest']), ('components', []), ('components', ['rustc-dev']), ('components', ['rustc-dev', 'llvm-tools', 'clippy']), ('packages', []), ('rust-host', 'x86_64-unknown-linux-gnu')]:
                changed = copy.deepcopy(data)
                changed[name][field] = value
                with self.assertRaises(ValueError): catalog.validate(changed)


class Archives(unittest.TestCase):
    def test_extraction_rejects_traversal(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); archive=root/'archive.tar'
            with tarfile.open(archive,'w') as bundle:
                info=tarfile.TarInfo('../escape');info.size=4
                bundle.addfile(info,io.BytesIO(b'fail'))
            with self.assertRaises(tarfile.TarError): catalog.unpack(archive,root/'output')
            self.assertFalse((root/'escape').exists())

    def test_complete_tool_layout_survives_install_and_repeat(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);archive=root/'archive.tar'
            with tarfile.open(archive,'w') as bundle:
                for name,content in [('tool/bin/compiler',b'compiler'),('tool/lib/runtime',b'runtime')]:
                    info=tarfile.TarInfo(name);info.size=len(content);info.mode=0o755
                    bundle.addfile(info,io.BytesIO(content))
            asset={'url':'https://example.test/tool.tar','sha256':'a'*64}
            def download(url,path,checksum): Path(path).write_bytes(archive.read_bytes())
            with patch.object(catalog,'download',side_effect=download) as fetch:
                directory=catalog.install_archive('tool',asset,root/'prefix')
                self.assertEqual((directory/'lib/runtime').read_bytes(),b'runtime')
                self.assertEqual(catalog.install_archive('tool',asset,root/'prefix'),directory)
                self.assertEqual(fetch.call_count,1)

    def test_zip_extraction_preserves_executable_permissions(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / 'tool.zip'
            info = zipfile.ZipInfo('tool')
            info.external_attr = 0o100755 << 16
            with zipfile.ZipFile(archive, 'w') as bundle:
                bundle.writestr(info, b'#!/bin/sh\nexit 0\n')
            catalog.unpack(archive, root / 'output')
            self.assertEqual(subprocess.run([str(root / 'output/tool')]).returncode, 0)

    def test_bad_checksum_does_not_publish_download(self):
        response=io.BytesIO(b'bad payload');response.url='https://example.test/asset'
        with tempfile.TemporaryDirectory() as temporary, patch.object(catalog.urllib.request,'urlopen',return_value=response):
            path=Path(temporary)/'download'
            with self.assertRaises(ValueError): catalog.download(response.url,path,'a'*64)
            self.assertFalse(path.exists())


if __name__ == '__main__': unittest.main()
