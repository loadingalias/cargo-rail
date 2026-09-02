#!/usr/bin/env python3
"""Regression tests for exact release component manifest verification."""

from __future__ import annotations

import hashlib
import importlib.util
import pathlib
import tempfile
import unittest


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
VERIFIER_PATH = REPOSITORY_ROOT / "scripts" / "ci" / "verify-release-components.py"

SPEC = importlib.util.spec_from_file_location("verify_release_components", VERIFIER_PATH)
if SPEC is None or SPEC.loader is None:
  raise RuntimeError(f"cannot load {VERIFIER_PATH}")
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)


def write_inventory(directory: pathlib.Path, target: str, version: str, surface: bool) -> None:
  lines = [f"cargo-rail-components-v1\t{version}\t{target}"]
  for name, capability in sorted(VERIFIER.expected_inventory(target, surface).items()):
    payload = f"{target}:{name}\n".encode()
    (directory / name).write_bytes(payload)
    lines.append(f"{name}\t{hashlib.sha256(payload).hexdigest()}\t{len(payload)}\t{capability}")
  (directory / "cargo-rail-components-v1.tsv").write_text(
    "\n".join(lines) + "\n",
    encoding="ascii",
  )


class ReleaseComponentTests(unittest.TestCase):
  def test_exact_inventory_is_accepted_on_unix_and_windows(self) -> None:
    cases = (
      ("x86_64-unknown-linux-gnu", False),
      ("aarch64-pc-windows-msvc", True),
    )
    for target, surface in cases:
      with self.subTest(target=target, surface=surface), tempfile.TemporaryDirectory() as temporary_text:
        temporary = pathlib.Path(temporary_text)
        write_inventory(temporary, target, "0.26.0", surface)
        VERIFIER.verify(temporary, target, "0.26.0", surface)

  def test_missing_component_is_rejected_before_byte_verification(self) -> None:
    with tempfile.TemporaryDirectory() as temporary_text:
      temporary = pathlib.Path(temporary_text)
      target = "x86_64-unknown-linux-gnu"
      write_inventory(temporary, target, "0.26.0", True)
      manifest = temporary / "cargo-rail-components-v1.tsv"
      lines = manifest.read_text(encoding="ascii").splitlines()
      manifest.write_text("\n".join(lines[:-1]) + "\n", encoding="ascii")
      with self.assertRaisesRegex(RuntimeError, "exact platform inventory"):
        VERIFIER.verify(temporary, target, "0.26.0", True)


if __name__ == "__main__":
  unittest.main()
