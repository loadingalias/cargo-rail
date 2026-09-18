"""Exercise the independent compiler adapter packager."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    'package_compiler_adapter', ROOT / 'scripts/package-compiler-adapter.py'
)
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class CompilerAdapterPackage(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / 'source.json'
        self.destination = self.root / 'release'
        self.pack = {
            'version': 3,
            'fact_protocol': 3,
            'native_input_protocol': 2,
            'rustc': {'minimum_release': '1.98.0-nightly', 'minimum_commit_date': '2026-06-30'},
            'files': [{'path': 'source.rs', 'hex': '00'}],
        }

    def write(self):
        self.source.write_bytes(json.dumps(self.pack, separators=(',', ':')).encode() + b'\n')

    def test_pack_is_content_addressed_and_carries_a_verifiable_checksum(self):
        self.write()
        PACKAGER.package(self.source, self.destination)
        data = self.source.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        output = self.destination / f'cargo-rail-compiler-adapter-{digest}.json'
        self.assertEqual(output.read_bytes(), data)
        self.assertEqual((output.with_suffix('.json.sha256')).read_text(), f'{digest}  {output.name}\n')

    def test_incompatible_protocol_contract_publishes_nothing(self):
        self.pack['version'] = 2
        self.write()
        with self.assertRaisesRegex(ValueError, 'contract is incompatible'):
            PACKAGER.package(self.source, self.destination)
        self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
