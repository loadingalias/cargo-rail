"""Exercise release asset recovery through the workflow's actual shell block."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseRecovery(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.assets = self.root / 'release'
        self.remote = self.root / 'remote'
        self.assets.mkdir()
        self.remote.mkdir()
        for name in ['linux.zip', 'arm-linux.zip', 'windows.zip', 'macos.zip', 'cargo-rail-0.26.0.crate']:
            (self.assets / name).write_bytes(f'original {name}\n'.encode())
        (self.assets / 'SHA256SUMS').write_text(''.join(
            f'{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n'
            for asset in sorted(self.assets.iterdir())
        ))
        (self.root / 'notes.md').write_text('Release notes\n')
        binaries = self.root / 'bin'
        binaries.mkdir()
        gh = binaries / 'gh'
        gh.write_text(f'#!{sys.executable}\n' + textwrap.dedent('''
            import json, os, pathlib, shutil, sys
            root = pathlib.Path(os.environ['RUNNER_TEMP'])
            remote = root / 'remote'
            args = sys.argv[1:]
            with (root / 'calls.jsonl').open('a') as log:
                log.write(json.dumps(args) + '\\n')
            if args[0] == 'api':
                print(os.environ['GITHUB_SHA'])
            elif args[:2] == ['release', 'list']:
                if (root / 'exists').exists():
                    print('v0.26.0')
            elif args[:2] == ['release', 'view']:
                field = args[args.index('--json') + 1]
                print({'targetCommitish': os.environ['GITHUB_SHA'],
                       'body': (root / 'notes.md').read_text().strip(),
                       'isDraft': os.environ.get('RELEASE_DRAFT', 'true'),
                       'isPrerelease': 'false',
                       'assets': len(list(remote.iterdir()))}[field])
            elif args[:2] == ['release', 'download']:
                if not list(remote.iterdir()) or os.environ.get('FAIL_DOWNLOAD'):
                    sys.exit(1)
                destination = pathlib.Path(args[args.index('--dir') + 1])
                for asset in remote.iterdir():
                    shutil.copyfile(asset, destination / asset.name)
            elif args[:2] in [['release', 'create'], ['release', 'upload']]:
                (root / 'exists').touch()
                for arg in args[3:]:
                    if arg.startswith('--'):
                        break
                    asset = pathlib.Path(arg)
                    destination = remote / asset.name
                    if destination.exists():
                        sys.exit('refusing to overwrite an asset')
                    shutil.copyfile(asset, destination)
                    if os.environ.get('CORRUPT_UPLOAD'):
                        destination.write_bytes(b'corrupt upload')
                    if os.environ.get('FAIL_UPLOAD'):
                        sys.exit(1)
            else:
                sys.exit(f'unexpected gh command: {args}')
        '''))
        gh.chmod(0o755)
        self.environment = {
            'PATH': str(binaries) + os.pathsep + os.environ['PATH'],
            'LC_ALL': 'C',
            'RUNNER_TEMP': str(self.root),
            'GITHUB_REPOSITORY': 'example/cargo-rail',
            'GITHUB_SHA': 'a' * 40,
        }
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        step = workflow.split('      - name: Create or verify the release draft\n', 1)[1]
        self.script = textwrap.dedent(step.split('        run: |\n', 1)[1].split('      - name:', 1)[0])

    def seed_draft(self, missing=()):
        (self.root / 'exists').touch()
        for asset in self.assets.iterdir():
            if asset.name not in missing:
                shutil.copyfile(asset, self.remote / asset.name)

    def run_step(self, success=True):
        for name in ['existing-release', 'verified-release']:
            if (self.root / name).exists():
                shutil.rmtree(self.root / name)
        (self.root / 'calls.jsonl').write_text('')
        result = subprocess.run(
            ['bash', '--noprofile', '--norc', '-euo', 'pipefail', '-c', self.script],
            cwd=self.root, env=self.environment, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return [json.loads(line) for line in (self.root / 'calls.jsonl').read_text().splitlines()]

    def assert_complete(self):
        self.assertEqual(
            {asset.name: asset.read_bytes() for asset in self.remote.iterdir()},
            {asset.name: asset.read_bytes() for asset in self.assets.iterdir()},
        )

    def test_partial_create_recovers_and_completed_retry_uploads_nothing(self):
        self.environment['FAIL_UPLOAD'] = '1'
        self.run_step(success=False)
        self.assertTrue((self.root / 'exists').exists())
        self.assertEqual(len(list(self.remote.iterdir())), 1)
        missing = sorted(str(asset) for asset in self.assets.iterdir() if not (self.remote / asset.name).exists())
        del self.environment['FAIL_UPLOAD']
        calls = self.run_step()
        self.assertEqual([call for call in calls if call[:2] == ['release', 'upload']],
                         [['release', 'upload', 'v0.26.0', *missing]])
        self.assert_complete()
        calls = self.run_step()
        self.assertFalse(any(call[:2] in [['release', 'create'], ['release', 'upload']] for call in calls))

    def test_missing_checksums_are_restored(self):
        self.seed_draft(missing=['SHA256SUMS'])
        self.run_step()
        self.assert_complete()

    def test_empty_draft_recovers(self):
        self.seed_draft(missing=[asset.name for asset in self.assets.iterdir()])
        self.run_step()
        self.assert_complete()

    def test_conflicting_existing_asset_stops_before_upload(self):
        self.seed_draft(missing=['linux.zip'])
        (self.remote / 'windows.zip').write_bytes(b'different build')
        calls = self.run_step(success=False)
        self.assertFalse(any(call[:2] == ['release', 'upload'] for call in calls))
        self.assertFalse((self.remote / 'linux.zip').exists())

    def test_download_failure_is_not_treated_as_missing_assets(self):
        self.seed_draft(missing=['linux.zip'])
        self.environment['FAIL_DOWNLOAD'] = '1'
        calls = self.run_step(success=False)
        self.assertFalse(any(call[:2] == ['release', 'upload'] for call in calls))

    def test_published_release_is_verified_but_never_repaired(self):
        self.seed_draft()
        self.environment['RELEASE_DRAFT'] = 'false'
        self.run_step()
        (self.remote / 'linux.zip').unlink()
        calls = self.run_step(success=False)
        self.assertFalse(any(call[:2] == ['release', 'upload'] for call in calls))

    def test_partial_repair_can_be_retried(self):
        self.seed_draft(missing=['linux.zip', 'windows.zip'])
        self.environment['FAIL_UPLOAD'] = '1'
        self.run_step(success=False)
        self.assertTrue((self.remote / 'linux.zip').exists())
        self.assertFalse((self.remote / 'windows.zip').exists())
        del self.environment['FAIL_UPLOAD']
        self.run_step()
        self.assert_complete()

    def test_final_verification_rejects_corrupt_upload(self):
        self.seed_draft(missing=['linux.zip'])
        self.environment['CORRUPT_UPLOAD'] = '1'
        self.run_step(success=False)
        self.assertEqual((self.remote / 'linux.zip').read_bytes(), b'corrupt upload')


if __name__ == '__main__':
    unittest.main()
