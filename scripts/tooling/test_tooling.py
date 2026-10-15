"""Behavior checks for provisioning and stable release updates."""
import copy
import io
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

import tomlkit
import catalog
import fingerprint
import update
import verify


class RepositoryHygiene(unittest.TestCase):
    def test_python_bytecode_is_ignored_independent_of_interpreter_version(self):
        subprocess.run(
            [
                'git',
                'check-ignore',
                '--quiet',
                '--no-index',
                'scripts/tooling/__pycache__/compiler_support.cpython-999.pyc',
            ],
            cwd=catalog.ROOT,
            check=True,
        )

    def test_uv_environment_is_ignored(self):
        # CI fails on untracked files, and every `uv run` creates the project environment.
        subprocess.run(['git', 'check-ignore', '--quiet', '--no-index', '.venv/bin/python'], cwd=catalog.ROOT, check=True)


class CatalogCommand(unittest.TestCase):
    def test_empty_tool_inventory_emits_no_shell_array_element(self):
        output = subprocess.run(
            [sys.executable, str(Path(catalog.__file__).resolve()), 'select', 'aarch64-linux', 'package', 'targets'],
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


class CacheQualification(unittest.TestCase):
    def test_required_tests_cannot_be_missing_or_ignored(self):
        path = Path(catalog.__file__).resolve().parents[1] / 'check-cache-host.py'
        spec = importlib.util.spec_from_file_location('cache_host', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for available, ignored, accepted in [('required: test\n', '', True),
                                              ('different: test\n', '', False),
                                              ('required: test\n', 'required: test\n', False)]:
            with self.subTest(available=available, ignored=ignored), patch.object(
                module.subprocess, 'check_output', side_effect=[available, ignored]
            ):
                if accepted:
                    module.validate_cases('test-binary', ['required'])
                else:
                    with self.assertRaisesRegex(ValueError, 'missing or ignored'):
                        module.validate_cases('test-binary', ['required'])


class ReleaseSelection(unittest.TestCase):
    def test_release_asset_hashing_can_retain_a_stable_source_url(self):
        source = 'https://github.com/example/tool/releases/download/v1/tool.tar.gz'
        with patch.object(update, 'fetch', return_value=(b'archive', 'https://signed.example/temporary')):
            asset = update.pinned_url(source, preserve_source=True)
        self.assertEqual(asset['url'], source)
        self.assertEqual(asset['sha256'], update.hashlib.sha256(b'archive').hexdigest())

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
    def test_build_jobs_accept_tomlkit_integers(self):
        data = tomlkit.parse(catalog.CATALOG.read_text())
        self.assertIsNone(catalog.validate(data))

    def test_build_jobs_require_positive_integers(self):
        for platform in ('windows', 'aarch64-linux'):
            for value in (0, -1, True, '2'):
                data = catalog.read()
                data[platform]['cargo-build-jobs'] = value
                with self.subTest(platform=platform, value=value), self.assertRaisesRegex(ValueError, 'cargo-build-jobs'):
                    catalog.validate(data)

    def test_windows_build_jobs_are_required(self):
        data = catalog.read()
        del data['windows']['cargo-build-jobs']
        with self.assertRaisesRegex(ValueError, 'cargo-build-jobs'):
            catalog.validate(data)

    def test_full_ci_and_package_selections_preserve_native_build_tools(self):
        data = catalog.read()
        for platform in ('aarch64-linux', 'x86_64-linux', 'aarch64-win', 'x86_64-win'):
            with self.subTest(platform=platform):
                ci = catalog.selection(data, platform, 'ci')
                package = catalog.selection(data, platform, 'package')
                self.assertEqual(ci['cargo'], ['cargo-deny', 'cargo-nextest', 'just'])
                self.assertEqual(set(ci['components']), {'clippy', 'rustfmt', 'rustc-dev', 'llvm-tools'})
                self.assertEqual(package['cargo'], ['just'])
                self.assertEqual(package['components'], ['rustc-dev', 'llvm-tools'])
                required = {'rustup', 'uv', 'cargo-binstall', 'cmake'}
                if platform.endswith('-win'):
                    required |= {'llvm', 'git'}
                self.assertEqual(set(package['assets']), required)
                self.assertEqual(set(ci['assets']), required | ({'actionlint'} if platform == 'x86_64-linux' else set()))
                if platform == 'x86_64-linux':
                    self.assertIn('ripgrep', ci['packages'], 'tooling tests execute real manifest discovery through rg')
                if platform.endswith('-linux'):
                    self.assertFalse({'shellcheck', 'ripgrep', 'jq'} & set(package['packages']))
                # Integration tests build a foreign Linux target to prove cross-compiler diagnostics.
                foreign = {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'} - {data[platform]['rust-host']}
                self.assertTrue(foreign & set(ci['targets']), 'ci installs a foreign Linux target')
                self.assertEqual(package['targets'], [])
        self.assertEqual(data, catalog.read(), 'selection mutated the catalog')

    def test_cache_hosts_install_only_what_their_transferred_cache_tests_run(self):
        for platform in catalog.CACHE_HOSTS:
            with self.subTest(platform=platform):
                selected = catalog.selection(catalog.read(), platform, 'cache-host')
                # Clippy replays results; rustc-dev builds the driver. Tests and nextest arrive cross-built.
                self.assertEqual(set(selected['components']), {'clippy', 'rustc-dev'})
                self.assertEqual(selected['cargo'], [])
                self.assertEqual(selected['targets'], [])
                self.assertEqual(set(selected['assets']), {'rustup', 'uv'})
                self.assertEqual(set(selected['packages']), {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man'})
                for operation in ('ci', 'package'):
                    with self.assertRaises(ValueError):
                        catalog.selection(catalog.read(), platform, operation)
        with self.assertRaisesRegex(ValueError, 'cache hosts'):
            catalog.selection(catalog.read(), 'x86_64-linux', 'cache-host')

    def test_cross_build_installs_only_archive_build_tools_for_each_runner(self):
        for target in ('riscv64-linux', 's390x-linux', 'powerpc64le-linux'):
            with self.subTest(target=target):
                self.assertEqual(catalog.rust_channel('x86_64-linux', 'cross-build', target), catalog.rust_channel(target))
        self.assertEqual(catalog.rust_channel('x86_64-linux', 'ci'), catalog.rust_channel())
        with self.assertRaisesRegex(ValueError, 'cross-build targets'):
            catalog.rust_channel('x86_64-linux', 'cross-build', 'aarch64-linux')
        selected = catalog.selection(catalog.read(), 'x86_64-linux', 'cross-build')
        self.assertEqual(selected['components'], ['clippy', 'rustc-dev'])
        self.assertEqual(selected['cargo'], ['cargo-nextest', 'just'])
        self.assertEqual(set(selected['assets']), {'rustup', 'uv', 'cargo-binstall', 'cmake'})
        self.assertTrue({'git', 'git-man'} <= set(selected['packages']),
                        'Git and its version-coupled manual package must be installed together')
        for prefix in ('riscv64-linux-gnu', 's390x-linux-gnu', 'powerpc64le-linux-gnu'):
            self.assertTrue({f'gcc-{prefix}', f'g++-{prefix}'} <= set(selected['packages']), prefix)
        self.assertTrue({'libc6-dev-riscv64-cross', 'libc6-dev-s390x-cross', 'libc6-dev-ppc64el-cross'} <= set(selected['packages']))
        self.assertFalse({'shellcheck', 'ripgrep', 'openssl'} & set(selected['packages']))
        missing = catalog.read()
        missing['operations']['cross-build']['components'] = ['rustc-dev']
        with self.assertRaisesRegex(ValueError, 'cross-build must install'):
            catalog.validate(missing)
        with self.assertRaisesRegex(ValueError, 'requires x86_64-linux'):
            catalog.selection(catalog.read(), 'riscv64-linux', 'cross-build')

    def test_unknown_operation_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'unknown tooling operation'):
            catalog.selection(catalog.read(), 'x86_64-linux', 'typo')

    def test_selected_build_prerequisites_cannot_be_removed(self):
        for operation, field, name in [('package', 'components', 'rustc-dev'),
                                       ('package', 'components', 'llvm-tools'),
                                       ('package', 'assets', 'llvm'),
                                       ('package', 'assets', 'cmake'),
                                       ('ci', 'components', 'clippy'),
                                       ('ci', 'components', 'rustfmt'),
                                       ('ci', 'cargo', 'cargo-nextest'),
                                       ('ci', 'packages', 'build-essential'),
                                       ('ci', 'packages', 'git-man'),
                                       ('ci', 'packages', 'ripgrep'),
                                       ('package', 'packages', 'git-man'),
                                       ('cross-build', 'packages', 'git-man'),
                                       ('cross-build', 'packages', 'gcc-s390x-linux-gnu')]:
            with self.subTest(operation=operation, field=field, name=name):
                data = catalog.read()
                data['operations'][operation][field].remove(name)
                with self.assertRaises(ValueError):
                    catalog.validate(data)

    def test_selection_command_supplies_windows_package_inventory(self):
        output = subprocess.check_output(
            [sys.executable, str(Path(catalog.__file__).resolve()), 'select', 'x86_64-win', 'package'], text=True)
        selected = json.loads(output)
        self.assertEqual(selected['cargo'], ['just'])
        self.assertEqual(selected['components'], ['rustc-dev', 'llvm-tools'])
        self.assertEqual(set(selected['assets']), {'rustup', 'uv', 'cargo-binstall', 'llvm', 'cmake', 'git'})

    def test_rust_override_requires_a_dated_nightly(self):
        for channel in ('nightly', 'stable', 'nightly-latest'):
            data = catalog.read()
            data['riscv64-linux']['rust-channel'] = channel
            with self.assertRaisesRegex(ValueError, 'dated nightly'):
                catalog.validate(data)

    def test_cache_host_profiles_are_exact_and_cannot_grow_full_tooling(self):
        data = catalog.read()
        catalog.validate(data)
        for name in ('riscv64-linux', 's390x-linux', 'powerpc64le-linux'):
            for field, value in [('cargo', ['cargo-nextest']), ('cargo', ['just']), ('components', []),
                                 ('components', ['rustc-dev']), ('components', ['rustc-dev', 'llvm-tools', 'clippy']),
                                 ('packages', []), ('packages', ['build-essential', 'ca-certificates', 'cmake', 'curl',
                                                                 'git', 'git-man']),
                                 ('targets', ['x86_64-unknown-linux-gnu']), ('rust-host', 'x86_64-unknown-linux-gnu')]:
                changed = copy.deepcopy(data)
                changed[name][field] = value
                with self.assertRaises(ValueError): catalog.validate(changed)

    def test_every_platform_bootstraps_the_pinned_uv(self):
        for platform in (*catalog.PLATFORMS, 'aarch64-macos'):
            for change in ('missing', 'version', 'host'):
                data = catalog.read()
                assets = data[platform]['assets']
                if change == 'missing':
                    del assets['uv']
                elif change == 'version':
                    assets['uv']['url'] = assets['uv']['url'].replace(data['versions']['uv'], '0.0.1')
                else:
                    other = 'x86_64-unknown-linux-gnu' if platform != 'x86_64-linux' else 'aarch64-unknown-linux-gnu'
                    assets['uv']['url'] = re.sub(r'uv-[^/]+?\.(tar\.gz|zip)$', f'uv-{other}.tar.gz', assets['uv']['url'])
                with self.subTest(platform=platform, change=change), self.assertRaisesRegex(ValueError, 'uv'):
                    catalog.validate(data)

    def test_python_version_names_one_exact_release(self):
        for content in ('3.14.8\n', '3.14\n', '3.14.8', 'cpython@3.14.8\n'):
            with self.subTest(content=content), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / '.python-version').write_text(content)
                with patch.object(catalog, 'ROOT', root):
                    if content == '3.14.8\n':
                        catalog.validate(catalog.read())
                    else:
                        with self.assertRaisesRegex(ValueError, '.python-version'):
                            catalog.validate(catalog.read())

    def test_required_tools_must_report_their_exact_pin(self):
        data = catalog.read()
        version = data['cargo']['cargo-nextest']
        for report, accepted in [(f'cargo-nextest {version} (abc 2026-10-01)', True),
                                 (f'cargo-nextest {version}1', False), (f'cargo-nextest 1{version}', False),
                                 ('', False)]:
            with self.subTest(report=report), patch.object(catalog, 'version_report', return_value=report):
                if accepted:
                    catalog.require(data, ['cargo-nextest'])
                else:
                    with self.assertRaisesRegex(ValueError, f'cargo-nextest {re.escape(version)} is required'):
                        catalog.require(data, ['cargo-nextest'])
        with patch.object(catalog.subprocess, 'run', side_effect=FileNotFoundError):
            self.assertEqual(catalog.version_report('rustup'), '')
        with patch.object(catalog.subprocess, 'run') as run:
            run.return_value = subprocess.CompletedProcess([], 0, 'cargo-deny 1.0.0\nmore\n', '')
            self.assertEqual(catalog.version_report('cargo-deny'), 'cargo-deny 1.0.0')
            self.assertEqual(run.call_args.args[0], ['cargo', 'deny', '--version'])


class Archives(unittest.TestCase):
    def test_package_archive_installation_omits_workflow_validator(self):
        # The installers bootstrap rustup and uv themselves.
        with patch.object(sys, 'argv', ['catalog.py', 'install-archives', 'x86_64-linux', 'package', '/fixture']), \
             patch.object(catalog, 'install_archive', return_value=Path('/installed')) as install, \
             patch('sys.stdout', new_callable=io.StringIO):
            catalog.main()
        self.assertEqual([call.args[0] for call in install.call_args_list], ['cargo-binstall', 'cmake'])

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


class SelectedVerification(unittest.TestCase):
    def test_package_verification_requires_only_selected_components_and_tools(self):
        def run(*arguments, **kwargs):
            if arguments[:3] == ('rustup', 'component', 'list'):
                return 'rustc-dev-x86_64-unknown-linux-gnu\nllvm-tools-x86_64-unknown-linux-gnu\n'
            if Path(arguments[0]).name == 'rust-smoke':
                return 'native-rust-ok\n'
            if Path(arguments[0]).name == 'c-smoke':
                return 'native-c-ok\n'
            return ''
        with patch('sys.stdout', new_callable=io.StringIO), \
             patch.object(verify, 'verify_rust'), patch.object(verify, 'run', side_effect=run), \
             patch.object(verify, 'verify_version') as version:
            verify.verify('x86_64-linux', 'package')
        self.assertEqual([call.args[0] for call in version.call_args_list], ['rustup', 'cargo-binstall', 'cmake', 'uv', 'just'])


class ToolingLane(unittest.TestCase):
    def test_native_ci_configures_cache_before_plan_capture(self):
        lines = (catalog.ROOT / '.github/workflows/ci.yml').read_text().splitlines()
        start = lines.index('  native:')
        end = next(
            index for index, line in enumerate(lines[start + 1:], start + 1)
            if line.startswith('  ') and not line.startswith('    ') and line.endswith(':')
        )
        job = [line.strip() for line in lines[start:end]]
        cache = next(
            index for index, line in enumerate(job)
            if line.startswith('- uses: loadingalias/cargo-rail-action/cache@')
        )
        # The step that captures the plan publishes it as `steps.rail`.
        plan = job.index('id: rail')
        self.assertLess(cache, plan)
        self.assertNotIn('id: rail', job[plan + 1:])

    def test_shared_recipe_rejects_invalid_workflow(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shutil.copytree(catalog.ROOT / 'scripts', root / 'scripts')
            shutil.copytree(catalog.ROOT / '.config', root / '.config')
            for name in ('justfile', 'pyproject.toml', 'uv.lock', '.python-version'):
                shutil.copy(catalog.ROOT / name, root / name)
            (root / '.git').mkdir()
            workflows = root / '.github/workflows'
            workflows.mkdir(parents=True)
            (workflows / 'invalid.yml').write_text('''name: Invalid
on: push
jobs:
  check:
    runs-on: ubuntu-latest
    unexpected-tooling-key: true
    steps:
      - run: echo checked
''')
            environment = dict(os.environ)
            environment.pop('BASH_ENV', None)
            result = subprocess.run(['just', 'check-tooling'], cwd=root, env=environment,
                                    capture_output=True, text=True, timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('unexpected-tooling-key', result.stdout + result.stderr)


class Fingerprint(unittest.TestCase):
    INSTALLATIONS = (('x86_64-linux', 'ci'), ('x86_64-linux', 'package'), ('aarch64-linux', 'ci'),
                     ('riscv64-linux', 'cache-host'), ('x86_64-win', 'ci'), ('x86_64-win', 'package'))

    def test_each_platform_counts_only_its_installer_inputs(self):
        data = catalog.read()
        source = catalog.ROOT
        with tempfile.TemporaryDirectory() as temporary, patch.object(catalog, 'ROOT', Path(temporary)):
            root = Path(temporary)
            names = {name for platform, _ in self.INSTALLATIONS for name in fingerprint.inputs(platform)}
            for name in names | {'Cargo.toml', 'scripts/package-release.py', 'scripts/tooling/test_tooling.py',
                                 'scripts/tooling/aarch64-win.ps1'}:
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_bytes((source / name).read_bytes())

            def current():
                return {installation: fingerprint.fingerprint(data, *installation) for installation in self.INSTALLATIONS}
            original = current()
            self.assertEqual(len(set(original.values())), len(original))
            self.assertTrue(all(re.fullmatch('[0-9a-f]{64}', value) for value in original.values()))
            linux = {installation for installation in original if installation[0].endswith('-linux')}
            windows = set(original) - linux
            for name, changed in [('scripts/tooling/linux.sh', linux),
                                  ('scripts/tooling/uv.sh', linux),
                                  ('scripts/tooling/x86_64-linux.sh', {('x86_64-linux', 'ci'), ('x86_64-linux', 'package')}),
                                  ('scripts/tooling/riscv64-linux.sh', {('riscv64-linux', 'cache-host')}),
                                  ('scripts/tooling/windows.ps1', windows),
                                  ('scripts/tooling/x86_64-win.ps1', windows),
                                  ('scripts/tooling/aarch64-win.ps1', set()),
                                  ('scripts/tooling/catalog.py', set(original)),
                                  ('scripts/tooling/verify.py', set(original)),
                                  ('scripts/tooling/fingerprint.py', set(original)),
                                  ('.config/tooling.toml', set(original)),
                                  ('rust-toolchain.toml', set(original)),
                                  ('.python-version', set(original)),
                                  ('Cargo.toml', set()),
                                  ('scripts/package-release.py', set()),
                                  ('scripts/tooling/test_tooling.py', set())]:
                with self.subTest(name=name):
                    path = root / name
                    original_bytes = path.read_bytes()
                    path.write_bytes(original_bytes + b'\n# changed\n')
                    updated = current()
                    self.assertEqual({key for key in original if updated[key] != original[key]}, changed)
                    path.write_bytes(original_bytes)
            self.assertEqual(current(), original)

    def test_only_reusable_installations_have_a_fingerprint(self):
        data = catalog.read()
        for platform, operation, reason in [('x86_64-linux', 'cross-build', 'no reusable cross-build'),
                                            ('x86_64-linux', 'typo', 'unknown tooling operation'),
                                            ('x86_64-linux', 'cache-host', 'cache hosts'),
                                            ('riscv64-linux', 'ci', 'hosts that build'),
                                            ('x86_64-win', 'cache-host', 'cache hosts'),
                                            ('x86_64-macos', 'ci', 'unknown tooling platform')]:
            with self.subTest(platform=platform, operation=operation), self.assertRaisesRegex(ValueError, reason):
                fingerprint.fingerprint(data, platform, operation)

    def test_inputs_cover_every_script_an_installer_reads(self):
        scripts = {path.relative_to(catalog.ROOT).as_posix()
                   for directory in ('scripts', 'scripts/tooling')
                   for path in (catalog.ROOT / directory).iterdir() if path.is_file()}
        for platform in catalog.PLATFORMS:
            with self.subTest(platform=platform):
                inputs = set(fingerprint.inputs(platform))
                for name in inputs:
                    text = (catalog.ROOT / name).read_text()
                    if name.endswith('.py'):
                        # Python helpers import their siblings by module name.
                        for module in re.findall(r'^(?:from|import) (\w+)', text, re.MULTILINE):
                            if (catalog.ROOT / 'scripts/tooling' / f'{module}.py').is_file():
                                self.assertIn(f'scripts/tooling/{module}.py', inputs, f'{name} imports {module}')
                    else:
                        # Shell and PowerShell installers name the helpers they run.
                        for script in scripts:
                            if re.search(r'(?<![\w.-])' + re.escape(Path(script).name) + r'\b', text):
                                self.assertIn(script, inputs, f'{name} reads {script}')

    def test_every_host_names_the_inputs_of_an_operation(self):
        # No substitute host: the subcommand runs before any host check.
        def named(*arguments):
            return subprocess.run(['bash', str(catalog.ROOT / 'scripts/tooling/x86_64-linux.sh'), 'fingerprint', *arguments],
                                  capture_output=True, text=True)
        for operation in ('ci', 'package'):
            with self.subTest(operation=operation):
                result = named(operation)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertRegex(result.stdout, r'^[0-9a-f]{64}\n$')
                self.assertEqual(result.stdout, subprocess.check_output(
                    [sys.executable, str(catalog.ROOT / 'scripts/tooling/fingerprint.py'), 'x86_64-linux', operation], text=True))
        refused = named('cross-build')
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('no reusable cross-build installation', refused.stderr)
        # The installer has no default operation, so neither has its fingerprint.
        self.assertEqual(named().returncode, 64)

    @unittest.skipUnless(shutil.which('pwsh') or shutil.which('powershell'), 'requires PowerShell')
    def test_windows_front_door_names_the_same_fingerprint(self):
        shell = shutil.which('pwsh') or shutil.which('powershell')
        for operation in ('ci', 'package'):
            with self.subTest(operation=operation):
                result = subprocess.run([shell, '-NoProfile', '-NonInteractive', '-File',
                                         str(catalog.ROOT / 'scripts/tooling/x86_64-win.ps1'), 'fingerprint', operation],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(result.stdout.strip(), subprocess.check_output(
                    [sys.executable, str(catalog.ROOT / 'scripts/tooling/fingerprint.py'), 'x86_64-win', operation],
                    text=True).strip())


if __name__ == '__main__': unittest.main()
