"""Install a native release archive with cargo-binstall and source fallback disabled."""
import functools
import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
import zipfile
from pathlib import Path


class ArchiveHandler(http.server.SimpleHTTPRequestHandler):
    def copyfile(self, source, outputfile):
        try:
            super().copyfile(source, outputfile)
        except (BrokenPipeError, ConnectionResetError):
            # Binstall may close its availability probe before reading the archive.
            pass


def check(directory):
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline'], cwd=root,
    ))
    package = next(item for item in metadata['packages'] if Path(item['manifest_path']).resolve() == root / 'Cargo.toml')
    identity = dict(line.split(': ', 1) for line in subprocess.check_output(
        ['rustc', '-vV'], text=True, cwd=root,
    ).splitlines() if ': ' in line)
    target = identity['host']
    suffix = '.exe' if target.endswith('windows-msvc') else ''
    archive_path = Path(directory).resolve() / f'cargo-rail-{target}.zip'
    if not archive_path.is_file():
        raise ValueError(f'missing native release archive: {archive_path}')
    handler = functools.partial(ArchiveHandler, directory=str(archive_path.parent))
    with tempfile.TemporaryDirectory(prefix='cargo-rail-binstall-') as temporary, \
            http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            environment = dict(os.environ, CARGO_HOME=str(Path(temporary) / 'cargo-home'))
            for key in ['GH_TOKEN', 'GITHUB_TOKEN']:
                environment.pop(key, None)
            installed = Path(temporary) / 'bin'
            subprocess.run([
                'cargo', 'binstall', '--manifest-path', str(root / 'Cargo.toml'),
                '--version', package['version'], '--targets', target,
                '--pkg-url', f'http://127.0.0.1:{server.server_port}/{archive_path.name}',
                '--strategies', 'crate-meta-data', '--allow-insecure-http',
                '--no-discover-github-token', '--disable-telemetry', '--no-confirm',
                '--install-path', str(installed), package['name'],
            ], cwd=root, env=environment, check=True, timeout=120)
            with zipfile.ZipFile(archive_path) as archive:
                for item in package['targets']:
                    if 'bin' not in item['kind'] or item.get('required-features'):
                        continue
                    name = item['name'] + suffix
                    if (installed / name).read_bytes() != archive.read(f'cargo-rail/{name}'):
                        raise ValueError(f'cargo-binstall did not install the archived binary: {name}')
            version = subprocess.check_output([str(installed / ('cargo-rail' + suffix)), '--version'], text=True)
            if version.strip() != f'cargo-rail {package["version"]}':
                raise ValueError('installed executable version disagrees with Cargo metadata')
        finally:
            server.shutdown()
            thread.join()
    print(f'cargo-binstall installed and verified the native {target} archive without source fallback')


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: scripts/check-release-install.py ARCHIVE_DIRECTORY')
    try:
        check(sys.argv[1])
    except (OSError, ValueError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        raise SystemExit(str(error)) from error
