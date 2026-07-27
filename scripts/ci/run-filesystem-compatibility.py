#!/usr/bin/env python3
"""Verify the native filesystem semantics required by cargo-rail's test matrix."""

from __future__ import annotations

import argparse
import json
import os
import platform
import plistlib
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path


class FilesystemError(RuntimeError):
    """One filesystem capability disagreed with the declared CI profile."""


def run(argv: list[str]) -> str:
    completed = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if completed.returncode != 0:
        raise FilesystemError(
            f"command exited {completed.returncode}: {subprocess.list2cmdline(argv)}\n"
            f"stdout:\n{completed.stdout.decode(errors='replace')}\n"
            f"stderr:\n{completed.stderr.decode(errors='replace')}"
        )
    return completed.stdout.decode("utf-8").strip()


def filesystem_kind(root: Path) -> str:
    system = platform.system()
    if system == "Darwin":
        device = run(["df", "-P", str(root)]).splitlines()[-1].split()[0]
        raw = subprocess.run(
            ["diskutil", "info", "-plist", device],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if raw.returncode != 0:
            raise FilesystemError(raw.stderr.decode(errors="replace"))
        value = plistlib.loads(raw.stdout)
        kind = value.get("FilesystemType") or value.get("FilesystemName")
    elif system == "Linux":
        kind = run(["findmnt", "--noheadings", "--output", "FSTYPE", "--target", str(root)]).splitlines()[0]
    elif system == "Windows":
        escaped = str(root).replace("'", "''")
        kind = run(
            [
                "powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                f"(Get-Volume -FilePath '{escaped}').FileSystem",
            ]
        ).splitlines()[0]
    else:
        raise FilesystemError(f"unsupported native filesystem host: {system}")
    if not isinstance(kind, str) or not kind.strip():
        raise FilesystemError(f"could not identify the filesystem containing {root}")
    return kind.strip().lower()


def expected_bool(value: str) -> bool:
    if value == "true":
        return True
    if value == "false":
        return False
    raise FilesystemError(f"expected true or false, found {value!r}")


def verify_case_behavior(root: Path, expected: bool) -> bool:
    upper = root / "CargoRailCaseProbe"
    lower = root / "cargorailcaseprobe"
    upper.write_bytes(b"case\n")
    actual = not lower.exists()
    if actual != expected:
        raise FilesystemError(f"case sensitivity is {actual}, expected {expected}")
    upper.unlink()
    return actual


def verify_long_paths(root: Path) -> int:
    current = root
    component = "cargo-rail-long-path-component-0123456789"
    while len(str(current)) < 320:
        current /= component
        current.mkdir()
    artifact = current / "artifact.bin"
    artifact.write_bytes(b"long-path\n")
    if artifact.read_bytes() != b"long-path\n":
        raise FilesystemError("long-path bytes changed")
    return len(str(artifact))


def verify_links(root: Path) -> dict[str, int]:
    source = root / "hard-link-source"
    alias = root / "hard-link-alias"
    source.write_bytes(b"hard-link\n")
    os.link(source, alias)
    links = source.stat().st_nlink
    if links < 2 or alias.read_bytes() != b"hard-link\n":
        raise FilesystemError("hard-link identity was not preserved")
    alias.unlink()
    source.unlink()

    real_file = root / "symlink-file-source"
    file_link = root / "symlink-file"
    real_file.write_bytes(b"symlink\n")
    os.symlink("symlink-file-source", file_link, target_is_directory=False)
    if not file_link.is_symlink() or file_link.read_bytes() != b"symlink\n":
        raise FilesystemError("file symlink behavior changed")
    file_link.unlink()
    real_file.unlink()

    real_directory = root / "symlink-directory-source"
    directory_link = root / "symlink-directory"
    real_directory.mkdir()
    (real_directory / "entry").write_bytes(b"directory symlink\n")
    os.symlink("symlink-directory-source", directory_link, target_is_directory=True)
    if not directory_link.is_symlink() or (directory_link / "entry").read_bytes() != b"directory symlink\n":
        raise FilesystemError("directory symlink behavior changed")
    directory_link.unlink()
    shutil.rmtree(real_directory)
    return {"hard_links": links, "symlinks": 2}


def verify_modes(root: Path) -> str:
    path = root / "mode"
    path.write_bytes(b"mode\n")
    if os.name == "nt":
        os.chmod(path, stat.S_IREAD)
        if path.stat().st_mode & stat.S_IWRITE:
            raise FilesystemError("NTFS read-only mode was not observable")
        os.chmod(path, stat.S_IREAD | stat.S_IWRITE)
        result = "readonly"
    else:
        os.chmod(path, 0o755)
        if stat.S_IMODE(path.stat().st_mode) != 0o755:
            raise FilesystemError("executable mode was not preserved")
        os.chmod(path, 0o644)
        if stat.S_IMODE(path.stat().st_mode) != 0o644:
            raise FilesystemError("regular-file mode was not preserved")
        result = "0644/0755"
    path.unlink()
    return result


def verify_windows_paths(root: Path, unc_root: Path | None) -> dict[str, str]:
    if os.name != "nt":
        if unc_root is not None:
            raise FilesystemError("--unc-root is valid only on Windows")
        return {}

    drive = root.drive
    if not drive:
        raise FilesystemError(f"Windows profile root has no drive: {root}")
    path = root / "windows-path"
    path.write_bytes(b"windows path\n")
    forward_slashes = Path(str(path).replace("\\", "/"))
    if forward_slashes.read_bytes() != b"windows path\n":
        raise FilesystemError("Windows separator normalization changed bytes")
    resolved = str(path.resolve())
    verbatim = Path(resolved if resolved.startswith("\\\\?\\") else f"\\\\?\\{resolved}")
    if verbatim.read_bytes() != b"windows path\n":
        raise FilesystemError("Windows verbatim path changed bytes")

    junction_target = root / "junction-target"
    junction = root / "junction"
    junction_target.mkdir()
    (junction_target / "entry").write_bytes(b"junction\n")
    run(["cmd.exe", "/D", "/C", "mklink", "/J", str(junction), str(junction_target)])
    if (junction / "entry").read_bytes() != b"junction\n":
        raise FilesystemError("Windows junction changed bytes")
    os.rmdir(junction)
    if not junction_target.is_dir():
        raise FilesystemError("removing a junction removed its target")
    shutil.rmtree(junction_target)

    result = {"drive": drive, "verbatim": str(verbatim)}
    if unc_root is not None:
        marker = root / "unc-marker"
        marker.write_bytes(b"unc\n")
        if (unc_root / "unc-marker").read_bytes() != b"unc\n":
            raise FilesystemError("UNC access did not resolve to the qualified local root")
        (unc_root / "unc-round-trip").write_bytes(b"round-trip\n")
        if (root / "unc-round-trip").read_bytes() != b"round-trip\n":
            raise FilesystemError("UNC writes did not round-trip through the local drive path")
        result["unc"] = str(unc_root)
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--expected-filesystem", required=True)
    parser.add_argument("--expected-case-sensitive", choices=("true", "false"), required=True)
    parser.add_argument("--unc-root", type=Path)
    args = parser.parse_args()

    root = args.root.resolve()
    if not root.is_dir():
        raise FilesystemError(f"filesystem root is not a directory: {root}")
    actual_filesystem = filesystem_kind(root)
    if actual_filesystem != args.expected_filesystem.lower():
        raise FilesystemError(
            f"filesystem containing {root} is {actual_filesystem!r}, expected {args.expected_filesystem!r}"
        )

    probe = Path(tempfile.mkdtemp(prefix="cargo-rail-filesystem-", dir=root))
    try:
        case_sensitive = verify_case_behavior(probe, expected_bool(args.expected_case_sensitive))
        long_path_bytes = verify_long_paths(probe)
        links = verify_links(probe)
        modes = verify_modes(probe)
        unc_probe = args.unc_root / probe.relative_to(root) if args.unc_root is not None else None
        windows = verify_windows_paths(probe, unc_probe)
    finally:
        shutil.rmtree(probe)
    if probe.exists():
        raise FilesystemError(f"filesystem probe cleanup left {probe}")

    print(
        json.dumps(
            {
                "schema_version": 1,
                "filesystem": actual_filesystem,
                "case_sensitive": case_sensitive,
                "long_path_bytes": long_path_bytes,
                "modes": modes,
                **links,
                **windows,
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (FilesystemError, OSError) as error:
        print(f"filesystem compatibility: {error}", file=os.sys.stderr)
        raise SystemExit(2) from error
