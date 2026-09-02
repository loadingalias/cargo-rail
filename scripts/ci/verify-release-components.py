#!/usr/bin/env python3
"""Verify an extracted release component manifest and every declared byte."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import re


def main() -> None:
  parser = argparse.ArgumentParser()
  parser.add_argument("directory", type=pathlib.Path)
  parser.add_argument("--target", required=True)
  parser.add_argument("--version", required=True)
  parser.add_argument("--surface", choices=("true", "false"), required=True)
  arguments = parser.parse_args()

  directory = arguments.directory.resolve()
  manifest = directory / "cargo-rail-components-v1.tsv"
  lines = manifest.read_text(encoding="ascii").splitlines()
  if not lines or lines[0] != f"cargo-rail-components-v1\t{arguments.version}\t{arguments.target}":
    raise RuntimeError("release component manifest has incompatible authority")
  entries: dict[str, tuple[str, int, str]] = {}
  for line in lines[1:]:
    fields = line.split("\t")
    if len(fields) != 4:
      raise RuntimeError("release component manifest contains an invalid entry")
    name, digest, size_text, capability = fields
    if (
      name in entries
      or pathlib.PurePath(name).name != name
      or not re.fullmatch(r"[0-9a-f]{64}", digest)
      or not size_text.isascii()
      or not size_text.isdigit()
      or not capability
    ):
      raise RuntimeError("release component manifest contains invalid authority")
    entries[name] = (digest, int(size_text), capability)

  suffix = ".exe" if "-windows-" in arguments.target else ""
  expected = {
    f"cargo-rail{suffix}": "core",
    f"cargo-rail-compiler-observation{suffix}": "analysis",
    f"cargo-rail-native-rustc-wrapper{suffix}": "cache",
    f"cargo-rail-native-rustc-worker{suffix}": "cache",
    f"cargo-rail-distributed-worker{suffix}": "distributed",
  }
  if arguments.surface == "true":
    expected.update({
      f"cargo-rail-fact-driver{suffix}": "surface",
      "cargo-rail-fact-driver-source-v1.json": "surface-source",
    })
  if entries.keys() != expected:
    raise RuntimeError("release component manifest does not declare the exact platform inventory")
  for name, (digest, size, capability) in entries.items():
    if capability != expected[name]:
      raise RuntimeError(f"release component capability does not match its name: {name}")
    path = directory / name
    if path.is_symlink() or not path.is_file() or path.stat().st_size != size:
      raise RuntimeError(f"release component is missing or changed: {name}")
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
      raise RuntimeError(f"release component digest does not match: {name}")


if __name__ == "__main__":
  main()
