"""Declare the archive hosts from the release asset inventory."""

import hashlib
import json
import sys
import tomllib
from pathlib import Path

NAME = 'cargo-rail-archive-hosts-v1.json'


def archive_hosts(root):
    config = tomllib.loads((root / '.config/rail.toml').read_text())
    files = config['release']['assets']['cargo-rail']
    archives = {}
    for name in files:
        if not (name.startswith('cargo-rail-') and name.endswith('.zip')):
            continue
        host = name.removeprefix('cargo-rail-').removesuffix('.zip')
        if not host or host in archives:
            raise ValueError(f'archive inventory has an ambiguous host: {name}')
        archives[host] = name
    if not archives or NAME not in files:
        raise ValueError('release inventory must declare archives and the archive-host manifest')
    return {
        'schema_version': 1,
        'version': tomllib.loads((root / 'Cargo.toml').read_text())['package']['version'],
        'archives': dict(sorted(archives.items())),
        'unsupported_host_installation': 'source',
    }


def package(root, destination):
    manifest = archive_hosts(root)
    actual = {path.name for path in destination.glob('*.zip')}
    if actual != set(manifest['archives'].values()):
        raise ValueError('collected archives do not match the declared release hosts')
    content = (json.dumps(manifest, indent=2) + '\n').encode()
    with (destination / NAME).open('xb') as output:
        output.write(content)
    with (destination / 'SHA256SUMS').open('ab') as output:
        output.write(f'{hashlib.sha256(content).hexdigest()}  {NAME}\n'.encode())


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: scripts/package-archive-hosts.py ASSET_DIRECTORY')
    try:
        package(Path(__file__).resolve().parent.parent, Path(sys.argv[1]))
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error)) from error
