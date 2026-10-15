#!/usr/bin/env -S uv run python
"""Fingerprint what one platform's installation depends on, so a converged host or image can trust it.

usage: fingerprint.py PLATFORM OPERATION

Only the installers run this. It reads their own files, which no other lane needs.
"""
import hashlib
import sys

import catalog


def inputs(platform):
    """The files one platform's installer reads, relative to the repository root."""
    family = platform.rsplit('-', 1)[1]
    front_door = f'scripts/tooling/{platform}.' + ('ps1' if family == 'win' else 'sh')
    own = {'linux': ['scripts/tooling/linux.sh', 'scripts/tooling/uv.sh'], 'win': ['scripts/tooling/windows.ps1']}[family]
    # Every installer provisions the Python that .python-version names, through uv.
    return sorted([front_door, 'scripts/tooling/fingerprint.py', 'scripts/tooling/catalog.py',
                   'scripts/tooling/verify.py', '.config/tooling.toml', 'rust-toolchain.toml', '.python-version', *own])


def fingerprint(data, platform, operation):
    """Digest the installer's inputs, the operation, the required OS release and the platform.

    An edit to any other file, such as a test or another platform's installer,
    leaves the platform's hosts and images current.
    """
    if platform not in catalog.PLATFORMS:
        raise ValueError(f'unknown tooling platform: {platform}')
    # A cross-build installation depends on its target, so it always converges.
    if operation == 'cross-build':
        raise ValueError(f'{platform} has no reusable {operation} installation')
    catalog.selection(data, platform, operation)
    digest = hashlib.sha256()
    for name in inputs(platform):
        content = (catalog.ROOT / name).read_bytes()
        digest.update(f'{name}\0{len(content)}\0'.encode() + content)
    if platform.endswith('-win'):
        release = data[platform]['os']
    else:
        release = data[platform].get('ubuntu', data['linux']['ubuntu'])
    digest.update(f'{platform}\0{operation}\0{release}'.encode())
    return digest.hexdigest()


if __name__ == '__main__':
    if len(sys.argv) != 3:
        sys.exit(__doc__.split('\n\n')[1])
    try:
        print(fingerprint(catalog.read(), *sys.argv[1:]))
    except (ValueError, KeyError, OSError) as error:
        sys.exit(f'tooling: {error}')
