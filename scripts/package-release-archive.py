#!/usr/bin/env python3
"""Bind one release archive to its exact component inventory and bytes."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import shutil
import stat
import tarfile
import tempfile
import zipfile


COMPONENTS = {
  "cargo-rail": "core",
  "cargo-rail-compiler-observation": "analysis",
  "cargo-rail-native-rustc-wrapper": "cache",
  "cargo-rail-native-rustc-worker": "cache",
  "cargo-rail-distributed-worker": "distributed",
}


def main() -> None:
  parser = argparse.ArgumentParser()
  parser.add_argument("archive", type=pathlib.Path)
  parser.add_argument("--target", required=True)
  parser.add_argument("--version", required=True)
  parser.add_argument("--surface", choices=("true", "false"), required=True)
  arguments = parser.parse_args()

  archive = arguments.archive.resolve()
  with tempfile.TemporaryDirectory(prefix="cargo-rail-release-archive-") as temporary_text:
    temporary = pathlib.Path(temporary_text)
    root = temporary / "root"
    root.mkdir()
    if archive.name.endswith(".zip"):
      with zipfile.ZipFile(archive) as source:
        validate_names(source.namelist())
        source.extractall(root)
      archive_kind = "zip"
    elif archive.name.endswith(".tar.gz") or archive.name.endswith(".tgz"):
      with tarfile.open(archive, "r:gz") as source:
        validate_names(member.name for member in source.getmembers())
        source.extractall(root, filter="data")
      archive_kind = "tar"
    else:
      raise RuntimeError(f"unsupported release archive: {archive}")

    suffix = ".exe" if "-windows-" in arguments.target else ""
    expected = {f"{name}{suffix}": capability for name, capability in COMPONENTS.items()}
    if arguments.surface == "true":
      expected[f"cargo-rail-fact-driver{suffix}"] = "surface"
      expected["cargo-rail-fact-driver-source-v1.json"] = "surface-source"
    files = [path for path in root.rglob("*") if path.is_file() and not path.is_symlink()]
    by_name: dict[str, list[pathlib.Path]] = {}
    for path in files:
      by_name.setdefault(path.name, []).append(path)
    for name in expected:
      if len(by_name.get(name, [])) != 1:
        raise RuntimeError(f"archive must contain exactly one {name}")
    component_directory = by_name[f"cargo-rail{suffix}"][0].parent
    if any(by_name[name][0].parent != component_directory for name in expected):
      raise RuntimeError("release archive components are not adjacent")

    lines = [f"cargo-rail-components-v1\t{arguments.version}\t{arguments.target}"]
    for name, capability in sorted(expected.items()):
      path = by_name[name][0]
      digest = hashlib.sha256(path.read_bytes()).hexdigest()
      lines.append(f"{name}\t{digest}\t{path.stat().st_size}\t{capability}")
    manifest = component_directory / "cargo-rail-components-v1.tsv"
    manifest.write_text("\n".join(lines) + "\n", encoding="ascii")

    replacement = temporary / archive.name
    if archive_kind == "zip":
      with zipfile.ZipFile(replacement, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as output:
        for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
          if path.is_dir():
            continue
          if path.is_symlink() or not path.is_file():
            raise RuntimeError(f"archive contains a non-regular path: {path}")
          relative = path.relative_to(root).as_posix()
          info = zipfile.ZipInfo(relative)
          info.external_attr = (stat.S_IFREG | path.stat().st_mode) << 16
          info.compress_type = zipfile.ZIP_DEFLATED
          output.writestr(info, path.read_bytes(), compresslevel=9)
    else:
      with tarfile.open(replacement, "w:gz") as output:
        for path in sorted(root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()):
          if path.is_symlink() or (not path.is_dir() and not path.is_file()):
            raise RuntimeError(f"archive contains an unsupported path: {path}")
          output.add(path, arcname=path.relative_to(root), recursive=False)
    shutil.move(replacement, archive)


def validate_names(names) -> None:
  for name in names:
    path = pathlib.PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name or "\0" in name:
      raise RuntimeError(f"archive contains an unsafe path: {name}")


if __name__ == "__main__":
  main()
