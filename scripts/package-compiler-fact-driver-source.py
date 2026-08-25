#!/usr/bin/env python3
"""Build the authenticated, offline Surface driver source component."""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import tempfile


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
    # Emit generated text as bytes so the authenticated component is identical
    # on hosts whose text writers translate newlines.
    (package / "Cargo.toml").write_bytes(manifest.encode("utf-8"))
    shutil.copyfile(companion / "Cargo.lock", package / "Cargo.lock")
    shutil.copyfile(companion / "build.rs", package / "build.rs")
    for source in sorted((companion / "src").glob("*.rs")):
      shutil.copyfile(source, package / "src" / source.name)
    shutil.copyfile(
      repository / "src" / "compiler" / "fact_protocol.rs",
      root / "src" / "compiler" / "fact_protocol.rs",
    )
    # Start from the checked lockfile so Cargo preserves every selected runtime
    # version while pruning the removed development graph. Manufacture may
    # acquire an exact locked package that the preceding host build did not
    # need; the emitted source replacement makes consumer builds frozen and
    # offline. This also avoids a TOML parser dependency in the release
    # environment and keeps the component below its authenticated size bound.
    subprocess.run(
      [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--manifest-path",
        str(package / "Cargo.toml"),
      ],
      cwd=root,
      check=True,
      stdout=subprocess.DEVNULL,
    )
    # Vendor the pruned, locked runtime graph so every later build is frozen
    # and offline.
    subprocess.run(
      [
        "cargo",
        "vendor",
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
    (root / ".cargo" / "config.toml").write_bytes(
      b'[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
      b'[source.vendored-sources]\ndirectory = "vendor"\n',
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
