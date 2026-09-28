#!/usr/bin/env python3
"""Read the tooling catalog and install its verified, platform-specific archives."""
from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / '.config/tooling.toml'
PLATFORMS = ('aarch64-linux', 'x86_64-linux', 'aarch64-win', 'x86_64-win', 'riscv64-linux', 's390x-linux', 'powerpc64le-linux')
# Native validation hosts without release archives.
CACHE_HOSTS = ('riscv64-linux', 's390x-linux', 'powerpc64le-linux')
# GNU tool prefix of each host whose test archive x86-64 cross-builds.
CROSS_PREFIXES = {'riscv64-linux': 'riscv64-linux-gnu', 's390x-linux': 's390x-linux-gnu',
                  'powerpc64le-linux': 'powerpc64le-linux-gnu'}


def cross_target(target):
    if target not in CROSS_PREFIXES:
        raise ValueError(f'cross-build targets are {", ".join(CROSS_PREFIXES)}; found {target}')
    return target


def read(path=CATALOG):
    with Path(path).open('rb') as stream:
        return tomllib.load(stream)


def rust_channel(platform=None, operation=None, target=None):
    if operation == 'cross-build':
        platform = cross_target(target)
    if platform is not None:
        override = read()[platform].get('rust-channel')
        if override is not None:
            return override
    return read(ROOT / 'rust-toolchain.toml')['toolchain']['channel']


def selection(data, platform, operation):
    if operation not in data['operations']:
        raise ValueError(f'unknown tooling operation: {operation}')
    if operation == 'cross-build' and platform != 'x86_64-linux':
        raise ValueError('cross-build requires x86_64-linux')
    native = data[platform]
    if operation == 'package' and ('just' not in native['cargo'] or platform in CACHE_HOSTS):
        raise ValueError(f'{platform}: no package tool selection')
    if (operation == 'cache-host') != (platform in CACHE_HOSTS) and operation != 'cross-build':
        raise ValueError(f'{platform}: {operation} tooling is for '
                         + ('the IBM Z, IBM POWER, and RISC-V cache hosts' if operation == 'cache-host' else 'hosts that build'))
    policy = data['operations'][operation]
    selected = dict(native)
    for field in ('cargo', 'components', 'packages'):
        if field in native:
            selected[field] = [name for name in native[field] if name in policy[field]]
    selected['targets'] = [name for name in native.get('targets', []) if name in policy.get('targets', [])]
    selected['assets'] = {name: asset for name, asset in native['assets'].items() if name in policy['assets']}
    return selected


def download(url, destination, checksum):
    if not url.startswith('https://') or len(checksum) != 64:
        raise ValueError(f'invalid pinned download: {url}')
    request = urllib.request.Request(url, headers={'User-Agent': 'cargo-rail-tooling'})
    with urllib.request.urlopen(request, timeout=120) as response, Path(destination).open('wb') as output:
        if not response.url.startswith('https://'):
            raise ValueError('download redirected away from HTTPS')
        digest = hashlib.sha256()
        while block := response.read(1024 * 1024):
            digest.update(block)
            output.write(block)
    if digest.hexdigest() != checksum.lower():
        Path(destination).unlink()
        raise ValueError(f'checksum mismatch: {url}')


def unpack(archive, destination):
    destination = Path(destination)
    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as bundle:
            for member in bundle.infolist():
                resolved = (destination / member.filename).resolve()
                if not resolved.is_relative_to(destination.resolve()):
                    raise ValueError(f'archive path escapes destination: {member.filename}')
            bundle.extractall(destination)
            if os.name != 'nt':
                for member in bundle.infolist():
                    mode = (member.external_attr >> 16) & 0o777
                    if mode and not member.is_dir():
                        (destination / member.filename).chmod(mode)
    else:
        with tarfile.open(archive) as bundle:
            bundle.extractall(destination, filter='data')


def install_archive(name, asset, prefix):
    """Keep complete tool distributions; their adjacent libraries are required."""
    prefix = Path(prefix)
    destination = prefix / name / asset['sha256'][:16]
    receipt = destination / '.rail-installed'
    if not receipt.is_file():
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
            stage = Path(temporary)
            archive = stage / 'download'
            download(asset['url'], archive, asset['sha256'])
            unpack(archive, stage / 'unpacked')
            contents = stage / 'unpacked'
            children = list(contents.iterdir())
            if len(children) == 1 and children[0].is_dir():
                contents = children[0]
            if destination.exists():
                shutil.rmtree(destination)
            shutil.move(str(contents), destination)
            receipt.write_text(asset['sha256'] + '\n')
    return destination


def validate(data):
    bounded_platforms = {'windows': data['windows'], **{platform: data[platform] for platform in PLATFORMS}}
    for platform, config in bounded_platforms.items():
        jobs = config.get('cargo-build-jobs')
        if jobs is not None and (isinstance(jobs, bool) or not isinstance(jobs, int) or jobs < 1):
            raise ValueError(f'{platform}: cargo-build-jobs must be a positive integer')
    if 'cargo-build-jobs' not in data['windows']:
        raise ValueError('windows: cargo-build-jobs must be a positive integer')
    if set(data['operations']) != {'ci', 'package', 'cross-build', 'cache-host'}:
        raise ValueError('tooling operations must be ci, package, cross-build, and cache-host')
    for operation, policy in data['operations'].items():
        if operation not in ('cross-build', 'cache-host') and not {'rustc-dev', 'llvm-tools'} <= set(policy['components']):
            raise ValueError(f'{operation}: missing compiler components')
        for tool in policy['cargo']:
            if tool not in data['cargo']:
                raise ValueError(f'{operation}: no version for {tool}')
    for platform in PLATFORMS:
        config = data[platform]
        if 'rust-channel' in config and not re.fullmatch(r'nightly-\d{4}-\d{2}-\d{2}', config['rust-channel']):
            raise ValueError(f'{platform}: Rust override must name a dated nightly')
        arch, os_name = platform.split('-')
        arch = 'riscv64gc' if arch == 'riscv64' else arch
        host = f'{arch}-pc-windows-msvc' if os_name == 'win' else f'{arch}-unknown-linux-gnu'
        if config['rust-host'] != host or f'/{host}/rustup-init' not in config['assets']['rustup']['url']:
            raise ValueError(f'{platform}: mismatched native Rust host or bootstrap asset')
        for tool in config['cargo']:
            if tool not in data['cargo']:
                raise ValueError(f'{platform}: no version for {tool}')
        for name, asset in config['assets'].items():
            if not asset['url'].startswith('https://') or not re.fullmatch('[0-9a-f]{64}', asset['sha256']):
                raise ValueError(f'{platform}: invalid {name} asset')
        for operation in () if platform in CACHE_HOSTS else ('ci', 'package') if config['cargo'] else ('ci',):
            selected = selection(data, platform, operation)
            if platform == 'x86_64-linux' and operation == 'ci' and 'ripgrep' not in selected['packages']:
                raise ValueError('x86_64-linux/ci: tooling checks require ripgrep')
            components = {'rustc-dev', 'llvm-tools'}
            if operation == 'ci' and config['cargo']:
                components |= {'clippy', 'rustfmt'}
            if not components <= set(selected['components']):
                raise ValueError(f'{platform}/{operation}: missing compiler components')
            if operation == 'ci' and config['cargo'] and not selected['targets']:
                raise ValueError(f'{platform}/ci: integration tests require a foreign Linux target')
            if config['rust-host'] in selected['targets']:
                raise ValueError(f'{platform}/{operation}: the native host is not an added target')
            required = {'just', 'cargo-deny', 'cargo-nextest'} if operation == 'ci' else {'just'}
            if config['cargo'] and not required <= set(selected['cargo']):
                raise ValueError(f'{platform}/{operation}: missing required Cargo tools')
            prerequisites = set(config['assets']) - {'actionlint'}
            if not prerequisites <= set(selected['assets']):
                raise ValueError(f'{platform}/{operation}: missing native build/bootstrap asset')
            if platform.endswith('-linux') and not {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man', 'python3'} <= set(selected['packages']):
                raise ValueError(f'{platform}/{operation}: missing native build/bootstrap package')
    # Cache hosts run tests and tools that x86-64 cross-builds; they compile only the driver and test fixtures.
    runner_components = {'clippy', 'rustc-dev'}
    runner_packages = {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man', 'python3'}
    policy = data['operations']['cache-host']
    if (set(policy['components']) != runner_components or policy['cargo'] or set(policy['assets']) != {'rustup'}
            or set(policy['packages']) != runner_packages or policy.get('targets')):
        raise ValueError('cache-host must install only the runner compiler, Clippy, rustc-dev, and build packages')
    for platform in CACHE_HOSTS:
        config = data[platform]
        if (set(config['components']) != runner_components or config['cargo'] or config.get('targets')
                or set(config['assets']) != {'rustup'} or set(config['packages']) != runner_packages):
            raise ValueError(f'{platform}: the cache host installs only its compiler, Clippy, rustc-dev, and build packages')
        selection(data, platform, 'cache-host')

    cross = selection(data, 'x86_64-linux', 'cross-build')
    if set(cross['components']) != {'clippy', 'rustc-dev'} or set(cross['cargo']) != {'just', 'cargo-nextest'} or set(cross['assets']) != {'rustup', 'cargo-binstall', 'cmake'}:
        raise ValueError('cross-build must install only archive build tools')
    compilers = {f'{tool}-{prefix}' for prefix in CROSS_PREFIXES.values() for tool in ('gcc', 'g++')}
    libraries = {'libc6-dev-riscv64-cross', 'libc6-dev-s390x-cross', 'libc6-dev-ppc64el-cross'}
    if not {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man', 'python3', 'perl'} | compilers | libraries <= set(cross['packages']):
        raise ValueError('cross-build is missing cross compiler prerequisites')


def main():
    data = read()
    command, *args = sys.argv[1:]
    if command == 'get':
        value = data
        for key in args:
            value = value[key]
        if isinstance(value, list):
            for item in value:
                print(item)
        elif isinstance(value, dict):
            print(json.dumps(value))
        else:
            print(value)
    elif command == 'json':
        print(json.dumps(data))
    elif command == 'select':
        platform, operation, *keys = args
        value = selection(data, platform, operation)
        for key in keys:
            value = value[key]
        if isinstance(value, list):
            for item in value:
                print(item)
        else:
            print(json.dumps(value))
    elif command == 'rust-channel':
        print(rust_channel(*args))
    elif command == 'cross-prefix':
        print(CROSS_PREFIXES[cross_target(*args)])
    elif command == 'validate':
        validate(data)
        print('Tooling catalog passed')
    elif command == 'install-archive':
        platform, name, prefix = args
        print(install_archive(name, data[platform]['assets'][name], prefix))
    elif command == 'install-archives':
        platform, operation, prefix = args
        for name, asset in selection(data, platform, operation)['assets'].items():
            if name == 'rustup':
                continue
            directory = install_archive(name, asset, prefix)
            print(f'{name}\t{directory}')
    elif command == 'download':
        platform, name, destination = args
        asset = data[platform]['assets'][name]
        download(asset['url'], destination, asset['sha256'])
    else:
        raise ValueError(f'unknown catalog operation: {command}')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError) as error:
        sys.exit(f'tooling: {error}')
