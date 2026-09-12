#!/usr/bin/env python3
"""Package the native release build and its authenticated compiler components."""
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def package(destination):
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline'], cwd=root
    ))
    components = Path(metadata['target_directory']) / 'release'
    identity = dict(line.split(': ', 1) for line in subprocess.check_output(
        ['rustc', '-vV'], text=True, cwd=root
    ).splitlines() if ': ' in line)
    target = identity['host']
    version = tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']
    suffix = '.exe' if target.endswith('windows-msvc') else ''
    names = {
        'cargo-rail' + suffix: 'core',
        'cargo-rail-compiler-observation' + suffix: 'analysis',
        'cargo-rail-native-rustc-wrapper' + suffix: 'cache',
        'cargo-rail-native-rustc-worker' + suffix: 'cache',
        'cargo-rail-distributed-worker' + suffix: 'distributed',
        'cargo-rail-fact-driver' + suffix: 'surface',
        'cargo-rail-fact-driver-source-v1.json': 'surface-source',
        'LICENSE': 'license',
    }
    sources = {name: components / name for name in names}
    sources['LICENSE'] = root / 'LICENSE'
    authority = {}
    for line in (components / 'compiler-driver-authority.env').read_text().splitlines():
        words = shlex.split(line)
        if len(words) != 2 or words[0] != 'export' or '=' not in words[1]:
            raise ValueError('compiler component authority is not canonical')
        key, value = words[1].split('=', 1)
        if key in authority:
            raise ValueError('duplicate compiler component authority')
        authority[key] = value
    for key, field in [('RUSTC_HOST', 'host'), ('RUSTC_RELEASE', 'release'), ('RUSTC_COMMIT', 'commit-hash')]:
        if authority['CARGO_RAIL_FACT_DRIVER_' + key] != identity[field]:
            raise ValueError('compiler components do not match the selected native toolchain')
    files = {}
    for name, capability in sorted(names.items()):
        path = sources[name]
        limit = 64 * 1024 if capability == 'license' else 256 * 1024 * 1024
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
            raise ValueError(f'component is not a bounded regular file: {name}')
        files[name] = (digest(path), path.stat().st_size, capability)
    if sum(size for _, size, _ in files.values()) > 512 * 1024 * 1024:
        raise ValueError('component inventory exceeds the expanded archive bound')
    for prefix in ['CARGO_RAIL_FACT_DRIVER', 'CARGO_RAIL_FACT_DRIVER_SOURCE']:
        name = authority[prefix + '_FILE']
        if 'sha256:' + files[name][0] != authority[prefix + '_SHA256']:
            raise ValueError(f'component changed after authenticated preparation: {name}')
    binary = components / ('cargo-rail' + suffix)
    if subprocess.check_output([str(binary), 'rail', '--version'], text=True).strip() != f'cargo-rail {version}':
        raise ValueError('release executable version disagrees with Cargo.toml')
    manifest = f'cargo-rail-components-v1\t{version}\t{target}\n' + ''.join(
        f'{name}\t{sha}\t{size}\t{capability}\n' for name, (sha, size, capability) in files.items()
    )
    destination = Path(destination).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    archive_name = f'cargo-rail-{target}.zip'
    for name in [archive_name, 'SHA256SUMS']:
        if (destination / name).exists():
            raise ValueError(f'release output already exists: {destination / name}')
    with tempfile.TemporaryDirectory(prefix='.package-', dir=destination) as stage:
        archive_path = Path(stage) / archive_name
        with zipfile.ZipFile(archive_path, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            for name in files:
                archive.write(sources[name], f'cargo-rail/{name}')
            archive.writestr('cargo-rail/cargo-rail-components-v1.tsv', manifest)
        if archive_path.stat().st_size > 512 * 1024 * 1024:
            raise ValueError('release archive exceeds its byte bound')
        with zipfile.ZipFile(archive_path) as archive:
            for name, (sha, size, _) in files.items():
                with archive.open(f'cargo-rail/{name}') as entry:
                    if hashlib.file_digest(entry, 'sha256').hexdigest() != sha:
                        raise ValueError(f'archived component differs from its authority: {name}')
                if archive.getinfo(f'cargo-rail/{name}').file_size != size:
                    raise ValueError(f'archived component size changed: {name}')
        for name, (sha, size, _) in files.items():
            if sources[name].stat().st_size != size or digest(sources[name]) != sha:
                raise ValueError(f'component changed while packaging: {name}')
        checksum = Path(stage) / 'SHA256SUMS'
        checksum.write_text(f'{digest(archive_path)}  {archive_name}\n')
        # Exclusive creation preserves any output written by another packager.
        for source in [archive_path, checksum]:
            with source.open('rb') as reader, (destination / source.name).open('xb') as writer:
                shutil.copyfileobj(reader, writer)
    print(destination / archive_name)


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: scripts/package-release.py OUTPUT_DIRECTORY')
    try:
        package(sys.argv[1])
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
