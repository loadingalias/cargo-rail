#!/usr/bin/env python3
"""Test the compiler fact driver source manufacturing boundary."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("package-compiler-fact-driver-source.py")


def load_packager():
  spec = importlib.util.spec_from_file_location("package_compiler_fact_driver_source", SCRIPT)
  if spec is None or spec.loader is None:
    raise RuntimeError(f"cannot load source packager: {SCRIPT}")
  module = importlib.util.module_from_spec(spec)
  spec.loader.exec_module(module)
  return module


class SourcePackagerTests(unittest.TestCase):
  def test_manufacture_may_acquire_locked_inputs_but_emits_offline_source(self) -> None:
    packager = load_packager()
    commands: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[bytes]:
      commands.append(command)
      if command[1] == "vendor":
        vendor = pathlib.Path(command[-1]) / "fixture-0.0.0"
        vendor.mkdir(parents=True)
        (vendor / ".cargo-checksum.json").write_text('{"files":{},"package":null}\n', encoding="utf-8")
      return subprocess.CompletedProcess(command, 0)

    with tempfile.TemporaryDirectory(prefix="cargo-rail-source-packager-test-") as temporary_text:
      output = pathlib.Path(temporary_text) / "source.json"
      with mock.patch.object(packager.subprocess, "run", side_effect=run):
        with mock.patch.object(sys, "argv", [str(SCRIPT), str(output)]):
          packager.main()
      payload = json.loads(output.read_text(encoding="ascii"))

    metadata, vendor = commands
    self.assertEqual(metadata[:2], ["cargo", "metadata"])
    self.assertNotIn("--offline", metadata)
    self.assertEqual(vendor[:2], ["cargo", "vendor"])
    self.assertIn("--locked", vendor)
    self.assertIn("--versioned-dirs", vendor)
    files = {source["path"]: bytes.fromhex(source["hex"]) for source in payload["files"]}
    self.assertEqual(
      files[".cargo/config.toml"],
      b'[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
      b'[source.vendored-sources]\ndirectory = "vendor"\n',
    )
    self.assertIn("tools/compiler-fact-driver/Cargo.lock", files)
    self.assertNotIn(b"[dev-dependencies]", files["tools/compiler-fact-driver/Cargo.toml"])
    self.assertIn("vendor/fixture-0.0.0/.cargo-checksum.json", files)


if __name__ == "__main__":
  unittest.main()
