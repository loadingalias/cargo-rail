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


def read(path=CATALOG):
    with Path(path).open('rb') as stream:
        return tomllib.load(stream)


def rust_channel(platform=None, operation=None):
    if operation == 'riscv-build':
        platform = 'riscv64-linux'
    if platform is not None:
        override = read()[platform].get('rust-channel')
        if override is not None:
            return override
    return read(ROOT / 'rust-toolchain.toml')['toolchain']['channel']


def selection(data, platform, operation):
    if operation not in data['operations']:
        raise ValueError(f'unknown tooling operation: {operation}')
    if operation == 'riscv-build' and platform != 'x86_64-linux':
        raise ValueError('riscv-build requires x86_64-linux')
    native = data[platform]
    if operation == 'package' and 'just' not in native['cargo']:
        raise ValueError(f'{platform}: no package tool selection')
    policy = data['operations'][operation]
    selected = dict(native)
    for field in ('cargo', 'components', 'packages'):
        if field in native:
            selected[field] = [name for name in native[field] if name in policy[field]]
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
    jobs = data['windows'].get('cargo-build-jobs')
    if type(jobs) is not int or jobs < 1:
        raise ValueError('windows: cargo-build-jobs must be a positive integer')
    if set(data['operations']) != {'ci', 'package', 'riscv-build'}:
        raise ValueError('tooling operations must be ci, package, and riscv-build')
    for operation, policy in data['operations'].items():
        if operation != 'riscv-build' and not {'rustc-dev', 'llvm-tools'} <= set(policy['components']):
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
        for operation in ('ci', 'package') if config['cargo'] else ('ci',):
            selected = selection(data, platform, operation)
            if platform == 'x86_64-linux' and operation == 'ci' and 'ripgrep' not in selected['packages']:
                raise ValueError('x86_64-linux/ci: tooling checks require ripgrep')
            components = {'rustc-dev', 'llvm-tools'}
            if operation == 'ci' and config['cargo']:
                components |= {'clippy', 'rustfmt'}
            if not components <= set(selected['components']):
                raise ValueError(f'{platform}/{operation}: missing compiler components')
            required = {'just', 'cargo-deny', 'cargo-nextest'} if operation == 'ci' else {'just'}
            if config['cargo'] and not required <= set(selected['cargo']):
                raise ValueError(f'{platform}/{operation}: missing required Cargo tools')
            prerequisites = set(config['assets']) - {'actionlint'}
            if not prerequisites <= set(selected['assets']):
                raise ValueError(f'{platform}/{operation}: missing native build/bootstrap asset')
            if platform.endswith('-linux') and not {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man', 'python3'} <= set(selected['packages']):
                raise ValueError(f'{platform}/{operation}: missing native build/bootstrap package')
    for platform in ('riscv64-linux', 's390x-linux', 'powerpc64le-linux'):
        config = data[platform]
        if config['cargo'] or set(config['components']) != {'rustc-dev', 'llvm-tools'} or set(config['assets']) != ({'rustup', 'cargo-nextest'} if platform == 'riscv64-linux' else {'rustup'}):
            raise ValueError(f'{platform}: native cache validation requires its minimal compiler and runner tools')
        if not {'build-essential', 'ca-certificates', 'curl', 'git', 'python3'} <= set(config['packages']):
            raise ValueError(f'{platform}: missing native build/bootstrap package')

    cross = selection(data, 'x86_64-linux', 'riscv-build')
    if cross['components'] or set(cross['cargo']) != {'just', 'cargo-nextest'} or set(cross['assets']) != {'rustup', 'cargo-binstall', 'cmake'}:
        raise ValueError('riscv-build must install only archive build tools')
    if not {'build-essential', 'ca-certificates', 'curl', 'git', 'git-man', 'python3', 'perl', 'gcc-riscv64-linux-gnu',
            'g++-riscv64-linux-gnu', 'libc6-dev-riscv64-cross'} <= set(cross['packages']):
        raise ValueError('riscv-build is missing cross compiler prerequisites')


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
