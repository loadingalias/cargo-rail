"""Qualify one native release archive beyond `--version` on this host.

Usage: scripts/check-installation.py ARCHIVE [--previous ARCHIVE] [--old-toolchain TOOLCHAIN]

Every check runs with an isolated HOME and CARGO_HOME; the real rustup toolchains are only read.
The workspace fixture selects an older installed toolchain than Cargo-Rail's own `rust-version`,
so a passing run also proves that a prebuilt installation does not couple workspace MSRV to it.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = '.exe' if os.name == 'nt' else ''
RESULTS = []


def record(name, passed, detail=''):
    RESULTS.append((name, passed, detail))
    print(f'{"PASS" if passed else "FAIL"}  {name}{"  " + detail if detail else ""}', flush=True)


def run(command, cwd, env, timeout=900):
    return subprocess.run([str(part) for part in command], cwd=cwd, env=env, text=True,
                          capture_output=True, timeout=timeout)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def manifest(directory):
    lines = (directory / 'cargo-rail-components-v1.tsv').read_text().splitlines()
    header = lines[0].split('\t')
    entries = {fields[0]: fields[1] for fields in (line.split('\t') for line in lines[1:])}
    return header[1], header[2], entries


def install(archive, destination):
    """Extract the archive as a unit, preserving the executable bit on Unix."""
    destination.mkdir(parents=True)
    with zipfile.ZipFile(archive) as opened:
        for member in opened.infolist():
            path = Path(opened.extract(member, destination))
            if (member.external_attr >> 16) & 0o111:
                path.chmod(0o755)
    return destination / 'cargo-rail'


def status(binary, workspace, env):
    result = run([binary, 'rail', 'cache', 'status', '--json'], workspace, env)
    if result.returncode != 0:
        return {'error': result.stderr.strip()}
    return json.loads(result.stdout)['status']['installation']


def older_toolchain(requested):
    if requested:
        return requested
    minimum = tuple(int(part) for part in tomllib.loads((ROOT / 'Cargo.toml').read_text())
                    ['workspace']['package']['rust-version'].split('.'))
    listed = subprocess.check_output(['rustup', 'toolchain', 'list'], text=True).split('\n')
    candidates = []
    for line in listed:
        match = re.match(r'^(\d+)\.(\d+)\.(\d+)-\S+', line.strip())
        if match and tuple(int(part) for part in match.groups()) < minimum:
            candidates.append((tuple(int(part) for part in match.groups()), line.split()[0]))
    return min(candidates)[1] if candidates else None


def fixture(parent, toolchain, env):
    """A two-commit workspace whose own toolchain is older than Cargo-Rail's build version."""
    workspace = parent / 'work space'
    (workspace / 'crates/leaf/src').mkdir(parents=True)
    (workspace / 'crates/core/src').mkdir(parents=True)
    minor = '.'.join(toolchain.split('-')[0].split('.')[:2])
    (workspace / 'Cargo.toml').write_text("[workspace]\nmembers = ['crates/*']\nresolver = '2'\n")
    (workspace / 'rust-toolchain.toml').write_text(f"[toolchain]\nchannel = '{toolchain.split('-')[0]}'\n")
    (workspace / '.gitignore').write_text('target/\n')
    for name, dependency in [('core', ''), ('leaf', "core = { path = '../core' }\n")]:
        (workspace / f'crates/{name}/Cargo.toml').write_text(
            f"[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2021'\nrust-version = '{minor}'\n"
            f"[dependencies]\n{dependency}")
        (workspace / f'crates/{name}/src/lib.rs').write_text('pub fn value() -> u8 { 1 }\n')
    git = ['git', '-c', 'user.name=Installation Check', '-c', 'user.email=check@invalid', '-c', 'commit.gpgsign=false']
    for command in [['cargo', 'generate-lockfile', '--offline'], ['git', 'init', '-q', '-b', 'main'],
                    ['git', 'add', '.'], git + ['commit', '-qm', 'base']]:
        result = run(command, workspace, env)
        if result.returncode != 0:
            raise SystemExit(f'fixture command {command} failed: {result.stderr}')
    (workspace / 'crates/leaf/src/lib.rs').write_text('pub fn value() -> u8 { 2 }\n')
    for command in [['git', 'add', '.'], git + ['commit', '-qm', 'leaf']]:
        run(command, workspace, env)
    return workspace


def check(archive, previous, requested_toolchain):
    archive = Path(archive).resolve()
    sums = archive.parent / 'SHA256SUMS'
    if sums.is_file():
        expected = {line.split()[1].lstrip('*'): line.split()[0] for line in sums.read_text().splitlines() if line.strip()}
        record('archive checksum matches SHA256SUMS', expected.get(archive.name) == sha256(archive))
    else:
        record('archive checksum matches SHA256SUMS', False, 'no adjacent SHA256SUMS')
    toolchain = older_toolchain(requested_toolchain)
    if toolchain is None:
        record('older workspace toolchain is installed', False, 'pass --old-toolchain')
        return

    with tempfile.TemporaryDirectory(prefix='cargo-rail-installation-') as temporary:
        base = Path(temporary) / 'with spaces'
        home = base / 'home'
        home.mkdir(parents=True)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('CARGO_RAIL') and not key.startswith('CARGO_BUILD_RUSTC')
               and key not in {'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTUP_TOOLCHAIN', 'CARGO', 'RUSTC'}}
        env.update(HOME=str(home), CARGO_HOME=str(home / 'cargo home'),
                   RUSTUP_HOME=os.environ.get('RUSTUP_HOME', str(Path.home() / '.rustup')),
                   CARGO_TARGET_DIR=str(base / 'target'))
        Path(env['CARGO_HOME']).mkdir()
        installation = install(archive, base / 'install dir')
        version, target, components = manifest(installation)
        record('every archived component matches its manifest digest',
               all(sha256(installation / name) == digest for name, digest in components.items()))
        binary = installation / f'cargo-rail{EXE}'
        reported = run([binary, '--version'], base, env).stdout.strip()
        record('installed version is exact', reported == f'cargo-rail {version}', reported)

        workspace = fixture(base, toolchain, env)
        rustc = run(['rustc', '--version'], workspace, env).stdout.strip()
        record('workspace selects its own older toolchain', toolchain.split('-')[0] in rustc, rustc)
        planned = run([binary, 'rail', 'plan', '--since', 'HEAD~1', '--json'], workspace, env)
        plan = json.loads(planned.stdout) if planned.returncode == 0 else {}
        selected = plan.get('work', {}).get('cargo.test', {}).get('scope', {}).get('selection', {})
        record('plan selects the changed leaf package with the older toolchain',
               selected.get('cargo_args') == ['-p', 'leaf'], planned.stderr.strip()[-300:])
        full = run([binary, 'rail', 'plan', '--all', '--json'], workspace, env)
        record('plan --all succeeds in the fixture', full.returncode == 0, full.stderr.strip()[-300:])
        validated = run([binary, 'rail', 'config', 'validate', '--strict'], workspace, env)
        record('config validate --strict succeeds', validated.returncode == 0, validated.stderr.strip()[-300:])
        unified = run([binary, 'rail', 'unify', '--check'], workspace, env)
        record('unify --check runs with the older toolchain', unified.returncode in (0, 1), unified.stderr.strip()[-300:])

        launcher = base / 'bin'
        launcher.mkdir()
        (launcher / f'cargo-rail{EXE}').symlink_to(binary)
        setup = run([launcher / f'cargo-rail{EXE}', 'rail', 'cache', 'setup'], workspace, env)
        installed = status(binary, workspace, env)
        record('cache setup through a symlinked launcher discovers authenticated components',
               setup.returncode == 0 and installed.get('component_authentication') == 'authenticated'
               and installed.get('installation_integrity') == 'verified',
               setup.stderr.strip()[-300:] or json.dumps({key: installed.get(key) for key in
                                                          ['state', 'component_authentication', 'installation_integrity']}))

        config = Path(env['CARGO_HOME']) / 'config.toml'
        before = config.read_bytes() if config.exists() else b''
        modified = base / 'modified install'
        shutil.copytree(installation, modified / 'cargo-rail', symlinks=True)
        # The embedded authority declares the driver and its source; setup must reject a changed one.
        with open(modified / 'cargo-rail' / f'cargo-rail-fact-driver{EXE}', 'ab') as handle:
            handle.write(b'\0')
        rejected = run([modified / 'cargo-rail' / f'cargo-rail{EXE}', 'rail', 'cache', 'setup'], workspace, env)
        after = config.read_bytes() if config.exists() else b''
        record('a modified declared sibling is rejected before writes',
               rejected.returncode != 0 and before == after,
               rejected.stderr.strip().splitlines()[0] if rejected.stderr.strip() else 'no diagnostic')

        relocated = base / 'relocated install'
        shutil.move(str(base / 'install dir'), relocated)
        binary = relocated / 'cargo-rail' / f'cargo-rail{EXE}'
        # Setup copies components into CARGO_HOME, so moving the extracted archive cannot break them.
        moved = status(binary, workspace, env)
        record('moving the extracted installation keeps the installed cache verified',
               moved.get('installation_integrity') == 'verified' and moved.get('healthy') is True,
               json.dumps({key: moved.get(key) for key in ['state', 'installation_integrity', 'issues']})[:300])
        repaired = run([binary, 'rail', 'cache', 'setup'], workspace, env)
        repaired_status = status(binary, workspace, env)
        record('cache setup repairs a relocated installation',
               repaired.returncode == 0 and repaired_status.get('installation_integrity') == 'verified',
               repaired.stderr.strip()[-300:])
        relocated_plan = run([binary, 'rail', 'plan', '--since', 'HEAD~1', '--json'], workspace, env)
        record('the relocated installation still plans', relocated_plan.returncode == 0, relocated_plan.stderr.strip()[-300:])

        if previous:
            earlier = install(Path(previous).resolve(), base / 'previous install')
            earlier_binary = earlier / f'cargo-rail{EXE}'
            earlier_version = manifest(earlier)[0]
            for label, candidate in [(f'previous archive {earlier_version} replaces the candidate', earlier_binary),
                                     (f'candidate archive {version} upgrades it again', binary)]:
                result = run([candidate, 'rail', 'cache', 'setup'], workspace, env)
                current = status(candidate, workspace, env)
                record(f'cache setup: {label}',
                       result.returncode == 0 and current.get('installation_integrity') == 'verified',
                       result.stderr.strip()[-300:] or current.get('state', ''))

        removed = run([binary, 'rail', 'cache', 'uninstall'], workspace, env)
        after_uninstall = status(binary, workspace, env)
        wrapper_left = config.exists() and 'cargo-rail' in config.read_text()
        record('cache uninstall removes the wrapper',
               removed.returncode == 0 and after_uninstall.get('state') == 'not_installed' and not wrapper_left,
               removed.stderr.strip()[-300:])
        shutil.rmtree(relocated)
        outside = sorted(str(path.relative_to(home)) for path in home.rglob('*')
                         if not str(path.relative_to(home)).startswith('cargo home'))
        record('no state remains outside CARGO_HOME after uninstall', not outside, ', '.join(outside[:8]))
    return target


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('archive')
    parser.add_argument('--previous')
    parser.add_argument('--old-toolchain')
    arguments = parser.parse_args()
    check(arguments.archive, arguments.previous, arguments.old_toolchain)
    failed = [name for name, passed, _ in RESULTS if not passed]
    print(f'{len(RESULTS) - len(failed)} of {len(RESULTS)} installation checks passed')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
