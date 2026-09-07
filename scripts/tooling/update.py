#!/usr/bin/env python3
"""Local release selection for Cargo-Rail tooling and every Cargo manifest."""
from datetime import datetime, timezone
from functools import cache
from pathlib import Path
import hashlib
import json
import re
import subprocess
import sys
import tomllib
import urllib.request
import tomlkit

from catalog import ROOT, PLATFORMS, read, validate
REPOS = {'cargo-binstall': 'cargo-bins/cargo-binstall', 'cmake': 'Kitware/CMake', 'llvm': 'llvm/llvm-project',
         'git': 'git-for-windows/git', 'jq': 'jqlang/jq', 'powershell': 'PowerShell/PowerShell'}

def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'Cargo-Rail tooling (local release updater)'})
    with urllib.request.urlopen(request, timeout=120) as response:
        if not response.url.startswith('https://'):
            raise ValueError(f'non-HTTPS redirect: {url}')
        return response.read(), response.url


@cache
def api(url):
    if url.startswith('https://api.github.com/'):
        # gh owns credential handling; tokens never enter URLs or update output.
        result = subprocess.run(['gh', 'api', url.removeprefix('https://api.github.com/')],
                                capture_output=True, text=True)
        if result.returncode:
            raise ValueError(f'GitHub lookup failed: {url}\n{result.stderr.strip()}')
        return json.loads(result.stdout)
    return json.loads(fetch(url)[0])


def semver(value):
    match = re.fullmatch(r'v?(\d+)\.(\d+)\.(\d+)(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?', value)
    return tuple(map(int, match.groups())) if match else None


def eligible_release(versions, *, rust_version=None):
    candidates = []
    for version in versions:
        number = semver(version['num'])
        if number is None or version.get('yanked'):
            continue
        required = version.get('rust_version')
        if rust_version and required:
            required_tuple = tuple(map(int, required.split('.')))
            if required_tuple > tuple(map(int, rust_version.split('.'))):
                continue
        candidates.append((number, version['num']))
    if not candidates:
        raise ValueError('no eligible stable crate release')
    return max(candidates)[1].split('+', 1)[0]


def crate_version(name, rust_version=None):
    versions = api(f'https://crates.io/api/v1/crates/{name}')['versions']
    try:
        return eligible_release(versions, rust_version=rust_version)
    except ValueError as error:
        raise ValueError(f'{name}: {error}') from error


@cache
def release(repo):
    result = api(f'https://api.github.com/repos/{repo}/releases/latest')
    if result['prerelease'] or result['draft']:
        raise ValueError(f'{repo}: latest release is not stable')
    return result


def pinned_url(url):
    data, resolved = fetch(url)
    return {'url': resolved, 'sha256': hashlib.sha256(data).hexdigest()}


def github_asset(repo, name):
    matches = [asset for asset in release(repo)['assets'] if asset['name'] == name]
    if len(matches) != 1:
        raise ValueError(f'{repo}: required native release asset is missing: {name}')
    asset = matches[0]
    digest = asset.get('digest', '') or ''
    if re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
        return {'url': asset['browser_download_url'], 'sha256': digest.removeprefix('sha256:')}
    return pinned_url(asset['browser_download_url'])


def dependency_tables(document):
    for key in ('dependencies', 'dev-dependencies', 'build-dependencies'):
        if key in document:
            yield document[key]
    if 'workspace' in document and 'dependencies' in document['workspace']:
        yield document['workspace']['dependencies']
    for target in document.get('target', {}).values():
        yield from dependency_tables(target)


def run(*args, **kwargs):
    return subprocess.check_output(args, cwd=ROOT, text=True, **kwargs).strip()


def write(path, document):
    content = tomlkit.dumps(document)
    if content != path.read_text():
        print(f'Updating {path.relative_to(ROOT)}', flush=True)
        path.write_text(content)


def update_catalog():
    path = ROOT / '.config/tooling.toml'
    data = tomlkit.parse(path.read_text())
    for name in data['cargo']:
        data['cargo'][name] = crate_version(name)
    for name in data['updater']:
        data['updater'][name] = api(f'https://pypi.org/pypi/{name}/json')['info']['version']
    versions = data['versions']
    for name, repo in REPOS.items():
        versions[name] = release(repo)['tag_name'].removeprefix('v').removeprefix('jq-').removeprefix('llvmorg-')
    versions['rustup'] = tomllib.loads(fetch('https://static.rust-lang.org/rustup/release-stable.toml')[0].decode())['version']
    releases = api('https://www.python.org/api/v2/downloads/release/')
    versions['python'] = max((semver(item['name'].removeprefix('Python ')), item['name'].removeprefix('Python '))
        for item in releases if item['is_published'] and not item['pre_release']
        and semver(item['name'].removeprefix('Python ')))[1]
    # Refresh the same Ubuntu series; changing runner OS is a separate choice.
    snapshot = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    series = {(data[platform].get('ubuntu', data['linux']['ubuntu']),
               data[platform].get('codename', data['linux']['codename']))
              for platform in PLATFORMS if platform.endswith('-linux')}
    for ubuntu, codename in sorted(series):
        text = fetch(f'https://snapshot.ubuntu.com/ubuntu/{snapshot}/dists/{codename}/Release')[0].decode()
        if f'Version: {ubuntu}\n' not in text:
            raise ValueError(f'Ubuntu snapshot does not match the configured release: {ubuntu}')
    data['linux']['snapshot'] = snapshot
    channel_bytes, channel_url = fetch('https://aka.ms/vs/stable/channel')
    channel = json.loads(channel_bytes)
    vsman = next(item['payloads'][0] for item in channel['channelItems'] if item['id'] == 'Microsoft.VisualStudio.Manifests.VisualStudio')
    manifest = api(vsman['url'])
    sdk_ids = {item['id'] for item in manifest['packages'] if re.fullmatch(r'Microsoft.VisualStudio.Component.Windows11SDK.\d+', item['id'])}
    bootstrap = pinned_url('https://aka.ms/vs/stable/vs_buildtools.exe')
    data['windows'].update({'visual-studio': channel['info']['productDisplayVersion'],
        'build-version': channel['info']['buildVersion'], 'channel-url': channel_url,
        'channel-sha256': hashlib.sha256(channel_bytes).hexdigest(),
        'bootstrap-url': bootstrap['url'], 'bootstrap-sha256': bootstrap['sha256'],
        'sdk-component': max(sdk_ids, key=lambda item: int(item.rsplit('.', 1)[1]))})
    for name in PLATFORMS:
        platform = data[name]
        host = platform['rust-host']
        arch = host.split('-')[0]
        windows = 'windows' in host
        assets = platform['assets']
        rustup_url = f'https://static.rust-lang.org/rustup/archive/{versions["rustup"]}/{host}/rustup-init' + ('.exe' if windows else '')
        checksum = fetch(rustup_url + '.sha256')[0].decode().split()[0]
        if not re.fullmatch('[0-9a-f]{64}', checksum):
            raise ValueError('invalid rustup checksum')
        assets['rustup'] = {'url': rustup_url, 'sha256': checksum}
        if not platform['cargo']:
            continue
        assets['cargo-binstall'] = github_asset(REPOS['cargo-binstall'], f'cargo-binstall-{host}.' + ('zip' if windows else 'tgz'))
        cmake_arch = ('arm64' if arch == 'aarch64' else 'x86_64') if windows else arch
        assets['cmake'] = github_asset(REPOS['cmake'], f'cmake-{versions["cmake"]}-' + (f'windows-{cmake_arch}.zip' if windows else f'linux-{cmake_arch}.tar.gz'))
        if 'llvm' in assets:
            assets['llvm'] = github_asset(REPOS['llvm'], f'clang+llvm-{versions["llvm"]}-{host}.tar.xz')
        if windows:
            pyarch = 'arm64' if arch == 'aarch64' else 'amd64'
            assets['python'] = pinned_url(f'https://www.python.org/ftp/python/{versions["python"]}/python-{versions["python"]}-embed-{pyarch}.zip')
            git_version = versions['git'].replace('.windows.', '.')
            assets['git'] = github_asset(REPOS['git'], f'Git-{git_version}-' + ('arm64' if arch == 'aarch64' else '64-bit') + '.exe')
            assets['jq'] = github_asset(REPOS['jq'], f'jq-windows-{pyarch}.exe')
            assets['powershell'] = github_asset(REPOS['powershell'], f'PowerShell-{versions["powershell"]}-win-' + ('arm64' if arch == 'aarch64' else 'x64') + '.zip')
    validate(data)
    write(path, data)


def manifest_documents():
    # Includes all non-ignored manifests, including excluded workspaces and fixtures.
    paths = run('rg', '--files', '--hidden', '-0', '-g', 'Cargo.toml', '-g', '!.git',
                '-g', '!.agents', '-g', '!target', '-g', '!node_modules').split('\0')
    documents = {}
    for name in sorted(filter(None, paths)):
        path = ROOT / name
        if path.is_symlink() or not path.resolve().is_relative_to(ROOT):
            raise ValueError(f'manifest escapes repository or is a symlink: {name}')
        documents[path] = tomlkit.parse(path.read_text())
    return documents


def update_manifests(documents):
    for path, document in documents.items():
        rust_version = document.get('package', {}).get('rust-version')
        if isinstance(rust_version, dict):
            owner = next((documents[parent / 'Cargo.toml'] for parent in path.parents
                          if parent / 'Cargo.toml' in documents
                          and 'workspace' in documents[parent / 'Cargo.toml']), None)
            if owner is None:
                raise ValueError(f'cannot locate inherited Rust version: {path}')
            rust_version = owner['workspace']['package']['rust-version']
        for table in dependency_tables(document):
            for alias, dependency in list(table.items()):
                if isinstance(dependency, str):
                    name, previous = alias, dependency
                else:
                    if dependency.get('workspace') or dependency.get('path') or 'version' not in dependency:
                        continue
                    if dependency.get('registry') or dependency.get('git'):
                        raise ValueError(f'{path}: {alias}: non-crates.io version updates require an explicit source')
                    name, previous = dependency.get('package', alias), dependency['version']
                version = crate_version(name, rust_version=rust_version)
                version = ('=' if previous.startswith('=') else '') + version
                if isinstance(dependency, str):
                    table[alias] = version
                else:
                    dependency['version'] = version
    # Resolve every manifest version before writing any manifest.
    for path, document in documents.items():
        write(path, document)


def update_locks(documents):
    # Path dependencies resolve through their consumers, including excluded
    # fixture crates which Cargo deliberately cannot open as workspace roots.
    path_dependencies = {
        (path.parent / dependency['path'] / 'Cargo.toml').resolve()
        for path, document in documents.items()
        for table in dependency_tables(document)
        for dependency in table.values()
        if isinstance(dependency, dict) and 'path' in dependency
    }
    roots = set()
    for path, document in documents.items():
        if path.resolve() in path_dependencies and 'workspace' not in document:
            continue
        # Fixture Git URLs/revisions are materialized by the fixture harness.
        # Their dependency declarations above still receive the same updates.
        if any('__FIXTURE_' in parent.joinpath('Cargo.toml').read_text()
               for parent in path.parents if parent.joinpath('Cargo.toml') in documents):
            print(f'Fixture template: updated declarations only: {path.relative_to(ROOT)}')
            continue
        root = Path(run('cargo', 'locate-project', '--workspace', '--manifest-path', str(path), '--message-format', 'plain'))
        if not root.resolve().is_relative_to(ROOT):
            raise ValueError(f'workspace root escapes repository: {root}')
        roots.add(root)
    for root in sorted(roots):
        lock = root.with_name('Cargo.lock')
        previous = lock.read_bytes() if lock.exists() else None
        try:
            subprocess.run(['cargo', 'update', '--manifest-path', str(root)], cwd=ROOT, check=True)
        except BaseException:
            if previous is None:
                lock.unlink(missing_ok=True)
            else:
                lock.write_bytes(previous)
            raise


def update_rust():
    path = ROOT / 'rust-toolchain.toml'
    toolchain = tomlkit.parse(path.read_text())
    latest = tomllib.loads(fetch('https://static.rust-lang.org/dist/channel-rust-stable.toml')[0].decode())
    # Verify only the components each host actually installs.
    data = read()
    hosts = [(data[name]['rust-host'], data[name]['components']) for name in PLATFORMS]
    local_host = run('rustc', '-vV').split('host: ', 1)[1].splitlines()[0]
    hosts.append((local_host, toolchain['toolchain'].get('components', [])))
    aliases = {'clippy': 'clippy-preview', 'rustfmt': 'rustfmt-preview',
               'rust-analyzer': 'rust-analyzer-preview', 'llvm-tools': 'llvm-tools-preview'}
    for host, components in hosts:
        for component in ['rustc', 'cargo', 'rust-std', *components]:
            targets = latest['pkg'][aliases.get(component, component)]['target']
            if not targets.get(host, targets.get('*', {})).get('available'):
                raise ValueError(f'{latest["date"]}: {component} unavailable for {host}; toolchain not advanced')
    channel = latest['pkg']['rust']['version'].split()[0]
    subprocess.run(['rustup', 'toolchain', 'install', channel, '--profile', 'minimal',
                    '--component', 'clippy', '--component', 'rustfmt'], cwd=ROOT, check=True)
    toolchain['toolchain']['channel'] = channel
    write(path, toolchain)


def main():
    if sys.platform != 'darwin':
        raise ValueError('release updates run only on the local macOS workstation')
    if len(sys.argv) != 1:
        raise ValueError('usage: just update')
    update_catalog()
    update_rust()
    documents = manifest_documents()
    update_manifests(documents)
    update_locks(documents)
    for command in (['cargo', 'audit'], ['cargo', 'deny', '--locked', 'check', '-D', 'warnings', 'all']):
        subprocess.run(command, cwd=ROOT, check=True)
    print('Update complete')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, StopIteration, subprocess.CalledProcessError) as error:
        sys.exit(f'update: {error}')
