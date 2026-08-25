#!/usr/bin/env python3
"""Build the authenticated, offline Surface driver source component."""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import tempfile
import tomllib


def main() -> None:
  parser = argparse.ArgumentParser()
  parser.add_argument("output", type=pathlib.Path)
  arguments = parser.parse_args()

  repository = pathlib.Path(__file__).resolve().parent.parent
  companion = repository / "tools" / "compiler-fact-driver"
  with tempfile.TemporaryDirectory(prefix="cargo-rail-fact-driver-source-") as temporary_text:
    temporary = pathlib.Path(temporary_text)
    root = temporary / "source"
    package = root / "tools" / "compiler-fact-driver"
    (package / "src").mkdir(parents=True)
    (root / "src" / "compiler").mkdir(parents=True)
    (root / ".cargo").mkdir()

    manifest = (companion / "Cargo.toml").read_text(encoding="utf-8")
    manifest = manifest.split("\n[dev-dependencies]\n", maxsplit=1)[0].rstrip() + "\n"
    (package / "Cargo.toml").write_text(manifest, encoding="utf-8")
    shutil.copyfile(companion / "build.rs", package / "build.rs")
    for source in sorted((companion / "src").glob("*.rs")):
      shutil.copyfile(source, package / "src" / source.name)
    shutil.copyfile(
      repository / "src" / "compiler" / "fact_protocol.rs",
      root / "src" / "compiler" / "fact_protocol.rs",
    )
    subprocess.run(
      [
        "cargo",
        "generate-lockfile",
        "--offline",
        "--manifest-path",
        str(package / "Cargo.toml"),
      ],
      cwd=root,
      check=True,
    )
    checked_lock = tomllib.loads((companion / "Cargo.lock").read_text(encoding="utf-8"))
    checked_packages = {
      (entry["name"], entry["version"], entry.get("source"), entry.get("checksum"))
      for entry in checked_lock["package"]
    }
    checked_versions: dict[str, set[str]] = {}
    for entry in checked_lock["package"]:
      checked_versions.setdefault(entry["name"], set()).add(entry["version"])
    for _ in range(len(checked_packages)):
      runtime_lock = tomllib.loads((package / "Cargo.lock").read_text(encoding="utf-8"))
      runtime_packages = {
        (entry["name"], entry["version"], entry.get("source"), entry.get("checksum"))
        for entry in runtime_lock["package"]
      }
      drift = sorted(runtime_packages - checked_packages)
      if not drift:
        break
      name = drift[0][0]
      versions = checked_versions.get(name, set())
      if len(versions) != 1:
        raise RuntimeError(f"runtime driver dependency resolution drifted ambiguously: {drift}")
      subprocess.run(
        [
          "cargo",
          "update",
          "--offline",
          "--manifest-path",
          str(package / "Cargo.toml"),
          "-p",
          name,
          "--precise",
          next(iter(versions)),
        ],
        cwd=root,
        check=True,
      )
    else:
      raise RuntimeError("runtime driver dependency resolution did not converge on its checked Cargo.lock")
    subprocess.run(
      [
        "cargo",
        "vendor",
        "--offline",
        "--locked",
        "--versioned-dirs",
        "--manifest-path",
        str(package / "Cargo.toml"),
        str(root / "vendor"),
      ],
      cwd=root,
      check=True,
      stdout=subprocess.DEVNULL,
    )
    (root / ".cargo" / "config.toml").write_text(
      '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
      '[source.vendored-sources]\ndirectory = "vendor"\n',
      encoding="utf-8",
    )

    files: list[dict[str, str]] = []
    for source in sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix()):
      if source.is_symlink() or not source.is_file():
        if source.is_dir():
          continue
        raise RuntimeError(f"source bundle contains a non-regular path: {source}")
      files.append(
        {
          "path": source.relative_to(root).as_posix(),
          "hex": source.read_bytes().hex(),
        }
      )
    payload = json.dumps(
      {"version": 1, "files": files},
      ensure_ascii=True,
      separators=(",", ":"),
      sort_keys=True,
    ).encode("ascii")
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_bytes(payload)


if __name__ == "__main__":
  main()
