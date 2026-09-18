#!/usr/bin/env python3
"""Publish one content-addressed compiler adapter pack and its checksum."""

import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile


MAX_PACK_BYTES = 64 * 1024 * 1024


def package(source, destination):
    source = Path(source).resolve()
    if source.is_symlink() or not source.is_file() or not 0 < source.stat().st_size <= MAX_PACK_BYTES:
        raise ValueError('compiler adapter pack must be a bounded regular file')
    data = source.read_bytes()
    pack = json.loads(data)
    if (
        set(pack) != {'version', 'fact_protocol', 'native_input_protocol', 'rustc', 'files'}
        or pack['version'] != 3
        or not isinstance(pack['fact_protocol'], int) or pack['fact_protocol'] < 1
        or not isinstance(pack['native_input_protocol'], int) or pack['native_input_protocol'] < 1
        or set(pack['rustc']) != {'minimum_release', 'minimum_commit_date'}
        or not all(isinstance(value, str) and value for value in pack['rustc'].values())
        or not isinstance(pack['files'], list)
        or not pack['files']
    ):
        raise ValueError('compiler adapter pack contract is incompatible')
    if any(
        not isinstance(item, dict)
        or set(item) != {'path', 'hex'}
        or not isinstance(item['path'], str)
        or not item['path']
        or not isinstance(item['hex'], str)
        or len(item['hex']) % 2 != 0
        or any(character not in '0123456789abcdef' for character in item['hex'])
        for item in pack['files']
    ):
        raise ValueError('compiler adapter pack file inventory is invalid')
    paths = [item['path'] for item in pack['files']]
    if len(paths) != len(pack['files']) or paths != sorted(set(paths)):
        raise ValueError('compiler adapter pack inventory is not unique and sorted')
    digest = hashlib.sha256(data).hexdigest()
    name = f'cargo-rail-compiler-adapter-{digest}.json'
    checksum_name = f'{name}.sha256'
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    for output in (name, checksum_name):
        if (destination / output).exists():
            raise ValueError(f'compiler adapter output already exists: {destination / output}')
    with tempfile.TemporaryDirectory(prefix='.adapter-', dir=destination) as temporary:
        stage = Path(temporary)
        (stage / name).write_bytes(data)
        (stage / checksum_name).write_text(f'{digest}  {name}\n')
        for output in (name, checksum_name):
            with (stage / output).open('rb') as reader, (destination / output).open('xb') as writer:
                shutil.copyfileobj(reader, writer)
    print(destination / name)


if __name__ == '__main__':
    if len(sys.argv) != 3:
        raise SystemExit('usage: scripts/package-compiler-adapter.py SOURCE OUTPUT_DIRECTORY')
    try:
        package(sys.argv[1], sys.argv[2])
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        raise SystemExit(str(error)) from error
