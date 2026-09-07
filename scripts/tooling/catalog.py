#!/usr/bin/env python3
"""Read the tooling catalog and install its verified, platform-specific archives."""
from __future__ import annotations

import hashlib
import json
import os
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


def rust_channel():
    return read(ROOT / 'rust-toolchain.toml')['toolchain']['channel']


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
    for platform in PLATFORMS:
        config = data[platform]
        arch, os_name = platform.split('-')
        arch = 'riscv64gc' if arch == 'riscv64' else arch
        host = f'{arch}-pc-windows-msvc' if os_name == 'win' else f'{arch}-unknown-linux-gnu'
        if config['rust-host'] != host or f'/{host}/rustup-init' not in config['assets']['rustup']['url']:
            raise ValueError(f'{platform}: mismatched native Rust host or bootstrap asset')
        for tool in config['cargo']:
            if tool not in data['cargo']:
                raise ValueError(f'{platform}: no version for {tool}')
        for name, asset in config['assets'].items():
            if not asset['url'].startswith('https://') or not __import__('re').fullmatch('[0-9a-f]{64}', asset['sha256']):
                raise ValueError(f'{platform}: invalid {name} asset')
    for platform in ('riscv64-linux', 's390x-linux', 'powerpc64le-linux'):
        config = data[platform]
        if config['cargo'] or set(config['components']) != {'rustc-dev', 'llvm-tools'} or set(config['assets']) != {'rustup'}:
            raise ValueError(f'{platform}: native cache validation requires rustc-dev and llvm-tools without auxiliary Cargo tools')
        if not {'build-essential', 'ca-certificates', 'curl', 'git', 'python3'} <= set(config['packages']):
            raise ValueError(f'{platform}: missing native build/bootstrap package')


def main():
    data = read()
    command, *args = sys.argv[1:]
    if command == 'get':
        value = data
        for key in args:
            value = value[key]
        if isinstance(value, list):
            print('\n'.join(value))
        elif isinstance(value, dict):
            print(json.dumps(value))
        else:
            print(value)
    elif command == 'json':
        print(json.dumps(data))
    elif command == 'rust-channel':
        print(rust_channel())
    elif command == 'validate':
        validate(data)
        print('Tooling catalog passed')
    elif command == 'install-archives':
        platform, prefix = args
        for name, asset in data[platform]['assets'].items():
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
