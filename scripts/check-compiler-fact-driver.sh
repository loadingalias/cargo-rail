#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ ( ${1:-} == --prepare || ${1:-} == --prepare-source ) && $# == 2 ]]; then
  python3 - "$2" "$1" <<'PYTHON'
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib

root = Path.cwd()
source_only = sys.argv[2] == '--prepare-source'
destination = Path(sys.argv[1]).resolve()
destination.mkdir(parents=True, exist_ok=True)
manifest = root / 'tools/compiler-fact-driver/Cargo.toml'
environment = os.environ.copy()
environment.update(RUSTUP_AUTO_INSTALL='0', RUSTC_BOOTSTRAP='cargo_rail_fact_driver')
for name in ('RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'):
    environment[name] = ''
environment.pop('CARGO_ENCODED_RUSTFLAGS', None)

def run(arguments):
    return subprocess.check_output(arguments, env=environment, text=True).strip()

def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()

verbose = run(['rustc', '-vV'])
identity = dict(line.split(': ', 1) for line in verbose.splitlines() if ': ' in line)
sysroot = Path(run(['rustc', '--print', 'sysroot'])).resolve()
suffix = '.exe' if os.name == 'nt' else ''
driver_name = 'cargo-rail-fact-driver' + suffix
library_digest = None
if not source_only:
    libraries = sorted([*sysroot.glob('lib/librustc_driver-*.dylib'), *sysroot.glob('lib/librustc_driver-*.so'), *sysroot.glob('bin/rustc_driver-*.dll')])
    if len(libraries) != 1:
        raise SystemExit('prepare requires exactly one rustc_driver library in the selected toolchain; install its rustc-dev component first')
    development = sysroot / 'lib/rustlib' / identity['host'] / 'lib'
    if not any(path.suffix in ('.rmeta', '.rlib') for path in development.glob('librustc_hir-*')):
        raise SystemExit('prepare requires rustc-dev for the exact selected toolchain; no components were downloaded')
    library = libraries[0]
    library_digest = digest(library.read_bytes())
sources = [root / path for path in (
    'src/compiler/fact_protocol.rs', 'src/compiler/native_input_protocol.rs',
    'tools/compiler-fact-driver/Cargo.toml', 'tools/compiler-fact-driver/Cargo.lock',
    'tools/compiler-fact-driver/build.rs',
)]
sources.extend((root / 'tools/compiler-fact-driver/src').rglob('*.rs'))
source_bytes = {path.relative_to(root).as_posix(): path.read_bytes() for path in sorted(sources)}
source_identity = digest(b''.join(name.encode() + b'\0' + data + b'\0' for name, data in source_bytes.items()))
selection = {'source_identity': source_identity, 'rustc_verbose': verbose, 'sysroot': str(sysroot), 'compiler_library_digest': library_digest, 'source_only': source_only}
record_path = destination / '.cargo-rail-driver-preparation.json'
env_path = destination / 'compiler-driver-authority.env'
source_name = 'cargo-rail-fact-driver-source-v1.json'
names = (source_name, env_path.name) if source_only else (driver_name, source_name, env_path.name)
if record_path.is_file() and not record_path.is_symlink():
    record = json.loads(record_path.read_text())
    if record.get('selection') == selection and set(record.get('artifacts', {})) == set(names) and all(
        (destination / name).is_file() and not (destination / name).is_symlink()
        and digest((destination / name).read_bytes()) == expected
        for name, expected in record.get('artifacts', {}).items()
    ):
        print(f'Existing exact compiler components retained. Source {shlex.quote(str(env_path))} before building cargo-rail.')
        raise SystemExit(0)

environment['RUSTC'] = str(sysroot / 'bin' / ('rustc' + suffix))
run(['cargo', 'fetch', '--locked', '--manifest-path', str(manifest)])
build_target = Path(json.loads(run([
    'cargo', 'metadata', '--manifest-path', str(manifest), '--no-deps',
    '--format-version', '1', '--locked', '--offline',
]))['target_directory'])
built_driver = build_target / identity['host'] / 'release' / driver_name
with tempfile.TemporaryDirectory(prefix='.cargo-rail-driver-prepare-', dir=destination) as temporary:
    stage = Path(temporary)
    inventory = dict(source_bytes)
    for name, data in inventory.items():
        path = stage / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    runtime_manifest = stage / 'tools/compiler-fact-driver/Cargo.toml'
    development_dependencies = sorted(tomllib.loads(runtime_manifest.read_text()).get('dev-dependencies', {}))
    if development_dependencies:
        # cargo-rail: allow-unlocked-cargo: remove test-only dependencies from the private offline component snapshot.
        run([str(sysroot / 'bin' / ('cargo' + suffix)), 'remove', '--dev', '--offline', '--manifest-path', str(runtime_manifest), *development_dependencies])
        for name in ('tools/compiler-fact-driver/Cargo.toml', 'tools/compiler-fact-driver/Cargo.lock'):
            inventory[name] = (stage / name).read_bytes()
    vendor = stage / 'vendor'
    config = run(['cargo', 'vendor', '--locked', '--offline', '--versioned-dirs', '--manifest-path', str(runtime_manifest), str(vendor)])
    inventory['.cargo/config.toml'] = (config.replace(str(vendor), 'vendor') + '\n').encode()
    (stage / '.cargo').mkdir()
    (stage / '.cargo/config.toml').write_bytes(inventory['.cargo/config.toml'])
    if not source_only:
        flags = f'--remap-path-prefix={stage}=/cargo-rail-fact-driver --remap-path-scope=object'
        if identity['host'].endswith('-unknown-linux-musl'):
            flags += ' -C target-feature=-crt-static'
        environment['RUSTFLAGS'] = flags
        subprocess.run([
            str(sysroot / 'bin' / ('cargo' + suffix)), 'build', '--release', '--frozen',
            '--manifest-path', str(stage / 'tools/compiler-fact-driver/Cargo.toml'),
            '--target-dir', str(build_target), '--target', identity['host'],
        ], env=environment, check=True, cwd=stage)
    for path in sorted(vendor.rglob('*')):
        if path.is_symlink():
            raise SystemExit('vendored driver inputs must not contain symlinks')
        if path.is_file():
            inventory[path.relative_to(stage).as_posix()] = path.read_bytes()
    if len(inventory) > 10_000:
        raise SystemExit('driver source inventory exceeds its file bound')
    bundle = json.dumps({'version': 1, 'files': [{'path': name, 'hex': data.hex()} for name, data in sorted(inventory.items())]}, separators=(',', ':')).encode() + b'\n'
    if len(bundle) > 64 * 1024 * 1024:
        raise SystemExit('driver source bundle exceeds its byte bound')
    if source_bytes != {path.relative_to(root).as_posix(): path.read_bytes() for path in sorted(sources)} or (not source_only and library_digest != digest(library.read_bytes())) or verbose != run(['rustc', '-vV']):
        raise SystemExit('compiler driver inputs changed during preparation; retry after edits finish')
    (stage / source_name).write_bytes(bundle)
    authority = {
        'CARGO_RAIL_FACT_DRIVER_SOURCE_FILE': source_name,
        'CARGO_RAIL_FACT_DRIVER_SOURCE_SHA256': digest(bundle),
        'CARGO_RAIL_FACT_DRIVER_SOURCE_PROVENANCE': digest(bundle),
    }
    if not source_only:
        shutil.copyfile(built_driver, stage / driver_name)
        (stage / driver_name).chmod(0o700)
        authority.update({
            'CARGO_RAIL_FACT_DRIVER_FILE': driver_name,
            'CARGO_RAIL_FACT_DRIVER_SHA256': digest((stage / driver_name).read_bytes()),
            'CARGO_RAIL_FACT_DRIVER_PROVENANCE': digest(bundle),
            'CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE': identity['release'],
            'CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT': identity['commit-hash'],
            'CARGO_RAIL_FACT_DRIVER_RUSTC_HOST': identity['host'],
            'CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY': library.relative_to(sysroot).as_posix(),
            'CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256': library_digest,
            'CARGO_RAIL_TEST_FACT_DRIVER': str(destination / driver_name),
            'CARGO_RAIL_TEST_COMPONENT_BINARY': str(destination / ('cargo-rail' + suffix)),
        })
    (stage / env_path.name).write_text(''.join(f'export {name}={shlex.quote(value)}\n' for name, value in authority.items()))
    record = {'selection': selection, 'artifacts': {name: digest((stage / name).read_bytes()) for name in names}}
    for name in names:
        (stage / name).replace(destination / name)
    (stage / record_path.name).write_text(json.dumps(record, sort_keys=True, separators=(',', ':')) + '\n')
    (stage / record_path.name).replace(record_path)
print(f'Prepared authenticated compiler components. Source {shlex.quote(str(env_path))} before building cargo-rail.')
PYTHON
  exit 0
fi
if [[ $# != 0 ]]; then
  echo "usage: $0 [--prepare|--prepare-source <component-dir>]" >&2
  exit 2
fi

# rustc-dev must already be installed for the selected toolchain.
export RUSTC_BOOTSTRAP=cargo_rail_fact_driver
manifest=tools/compiler-fact-driver/Cargo.toml
cargo fmt --manifest-path "$manifest" --all -- --check
cargo clippy --manifest-path "$manifest" --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path "$manifest" --all-targets --all-features --locked
