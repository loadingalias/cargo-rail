#!/usr/bin/env python3
"""Regression tests for GNU archive runtime requirement parsing."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
VERIFIER_PATH = REPOSITORY_ROOT / "scripts" / "ci" / "verify-gnu-runtime.py"
FIXTURES = REPOSITORY_ROOT / "scripts" / "ci" / "fixtures"

SPEC = importlib.util.spec_from_file_location("verify_gnu_runtime", VERIFIER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {VERIFIER_PATH}")
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)


class GnuRuntimeRequirementTests(unittest.TestCase):
    def fixture(self, name: str) -> str:
        return (FIXTURES / name).read_text(encoding="utf-8")

    def test_numeric_requirements_include_patch_level_names(self) -> None:
        versions = VERIFIER.parse_required_glibc_versions(
            self.fixture("readelf-glibc-2.35.txt"), "fixture"
        )
        self.assertEqual(versions, {(2, 2), (2, 35)})

    def test_dt_relr_loader_requirement_is_below_current_floor(self) -> None:
        versions = VERIFIER.parse_required_glibc_versions(
            self.fixture("readelf-glibc-abi-dt-relr.txt"), "fixture"
        )
        self.assertIn((2, 36), versions)
        minimum_text, minimum = VERIFIER.runtime_authority()
        self.assertEqual((minimum_text, minimum), ("2.39", (2, 39)))
        VERIFIER.verify_required_glibc_floor(
            "fixture", versions, minimum, minimum_text
        )

    def test_requirement_above_current_floor_fails_closed(self) -> None:
        minimum_text, minimum = VERIFIER.runtime_authority()
        with self.assertRaisesRegex(RuntimeError, r"requires GLIBC_2.40"):
            VERIFIER.verify_required_glibc_floor(
                "fixture", {(2, 40)}, minimum, minimum_text
            )

    def test_unknown_glibc_namespace_fails_closed(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, r"unsupported GNU libc requirements: GLIBC_PRIVATE"
        ):
            VERIFIER.parse_required_glibc_versions(
                "Name: GLIBC_2.35\nName: GLIBC_PRIVATE\n", "fixture"
            )

    def test_manifest_cannot_relabel_an_executable_as_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_text:
            temporary = pathlib.Path(temporary_text)
            (temporary / "cargo-rail-components-v1.tsv").write_text(
                "cargo-rail-components-v1\t0.26.0\tx86_64-unknown-linux-gnu\n"
                "cargo-rail-native-rustc-wrapper\tignored\t0\tsurface-source\n",
                encoding="ascii",
            )
            with self.assertRaisesRegex(
                RuntimeError,
                r"cargo-rail-native-rustc-wrapper has capability 'surface-source'; expected 'cache'",
            ):
                VERIFIER.manifest_entries(
                    temporary, "x86_64-unknown-linux-gnu"
                )


if __name__ == "__main__":
    unittest.main()
