import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('archive_hosts', ROOT / 'scripts/package-archive-hosts.py')
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class ArchiveHostsTests(unittest.TestCase):
    def test_manifest_matches_the_collected_archives_and_checksum(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.config').mkdir()
            (root / 'Cargo.toml').write_text('[package]\nversion = "1.2.3"\n')
            (root / '.config/rail.toml').write_text(
                '[release.artifacts.cargo-rail.files]\n'
                '"cargo-rail-aarch64-apple-darwin.zip" = { target = "aarch64-apple-darwin" }\n'
                '"cargo-rail-archive-hosts-v1.json" = {}\n'
            )
            assets = root / 'assets'
            assets.mkdir()
            (assets / 'cargo-rail-aarch64-apple-darwin.zip').write_bytes(b'archive')
            (assets / 'SHA256SUMS').write_text('existing archive checksum\n')
            PACKAGER.package(root, assets)
            content = (assets / PACKAGER.NAME).read_bytes()
            self.assertEqual(json.loads(content), {
                'schema_version': 1, 'version': '1.2.3',
                'archives': {'aarch64-apple-darwin': 'cargo-rail-aarch64-apple-darwin.zip'},
                'unsupported_host_installation': 'source',
            })
            import hashlib
            self.assertEqual((assets / 'SHA256SUMS').read_text(),
                             f'existing archive checksum\n{hashlib.sha256(content).hexdigest()}  {PACKAGER.NAME}\n')

    def test_missing_or_undeclared_archives_cannot_claim_support(self):
        with tempfile.TemporaryDirectory() as temporary:
            assets = Path(temporary)
            with self.assertRaisesRegex(ValueError, 'do not match'):
                PACKAGER.package(ROOT, assets)
            self.assertFalse((assets / PACKAGER.NAME).exists())
            for archive in PACKAGER.archive_hosts(ROOT)['archives'].values():
                (assets / archive).write_bytes(b'archive')
            (assets / 'cargo-rail-undeclared.zip').write_bytes(b'archive')
            with self.assertRaisesRegex(ValueError, 'do not match'):
                PACKAGER.package(ROOT, assets)
            self.assertFalse((assets / PACKAGER.NAME).exists())
