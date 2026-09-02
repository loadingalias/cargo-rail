#!/usr/bin/env python3
"""Verify every executable component against the declared GNU runtime floor."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
AUTHORITY = REPOSITORY_ROOT / "distribution" / "gnu-runtime.json"
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
GLIBC_REQUIREMENT = re.compile(r"\bGLIBC_[A-Za-z0-9][A-Za-z0-9_.]*\b")
NUMERIC_GLIBC_REQUIREMENT = re.compile(
    r"^GLIBC_([0-9]+)\.([0-9]+)(?:\.[0-9]+)*$"
)
GLIBC_ABI_MINIMUMS = {"GLIBC_ABI_DT_RELR": (2, 36)}
COMPONENT_CAPABILITIES = {
    "cargo-rail": "core",
    "cargo-rail-compiler-observation": "analysis",
    "cargo-rail-native-rustc-wrapper": "cache",
    "cargo-rail-native-rustc-worker": "cache",
    "cargo-rail-distributed-worker": "distributed",
    "cargo-rail-fact-driver": "surface",
    "cargo-rail-fact-driver-source-v1.json": "surface-source",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def runtime_authority() -> tuple[str, tuple[int, int]]:
    value = json.loads(AUTHORITY.read_text(encoding="utf-8"))
    require(
        isinstance(value, dict)
        and set(value) == {"contract_version", "family", "minimum"}
        and value["contract_version"] == 1
        and value["family"] == "glibc"
        and isinstance(value["minimum"], str),
        "GNU runtime authority is invalid",
    )
    match = VERSION.fullmatch(value["minimum"])
    require(match is not None, "GNU runtime minimum is not a canonical major.minor version")
    return value["minimum"], (int(match.group(1)), int(match.group(2)))


def host_runtime() -> str:
    probe = subprocess.run(
        ["getconf", "GNU_LIBC_VERSION"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    require(probe.returncode == 0, "qualification host did not report GNU libc through getconf")
    fields = probe.stdout.strip().split()
    require(
        len(fields) == 2 and fields[0] == "glibc" and VERSION.fullmatch(fields[1]) is not None,
        f"qualification host reported an unsupported GNU runtime: {probe.stdout.strip()!r}",
    )
    return fields[1]


def manifest_entries(directory: pathlib.Path, target: str) -> list[tuple[pathlib.Path, str]]:
    manifest = directory / "cargo-rail-components-v1.tsv"
    lines = manifest.read_text(encoding="ascii").splitlines()
    require(bool(lines), "release component manifest is empty")
    header = lines[0].split("\t")
    require(
        len(header) == 3 and header[0] == "cargo-rail-components-v1" and header[2] == target,
        "release component manifest does not match the GNU target",
    )
    entries: list[tuple[pathlib.Path, str]] = []
    for line in lines[1:]:
        fields = line.split("\t")
        require(len(fields) == 4, "release component manifest contains an invalid entry")
        name, _, _, capability = fields
        require(pathlib.PurePath(name).name == name, "release component name is not local")
        expected_capability = COMPONENT_CAPABILITIES.get(name)
        require(
            expected_capability is not None,
            f"unknown release component: {name}",
        )
        require(
            capability == expected_capability,
            f"release component {name} has capability {capability!r}; "
            f"expected {expected_capability!r}",
        )
        if capability != "surface-source":
            entries.append((directory / name, capability))
    require(bool(entries), "release component manifest contains no GNU executables")
    return entries


def parse_required_glibc_versions(
    output: str, executable_name: str
) -> set[tuple[int, int]]:
    requirements = set(GLIBC_REQUIREMENT.findall(output))
    require(
        bool(requirements),
        f"release executable {executable_name} declares no GNU libc version requirements",
    )
    unsupported = sorted(
        requirement
        for requirement in requirements
        if NUMERIC_GLIBC_REQUIREMENT.fullmatch(requirement) is None
        and requirement not in GLIBC_ABI_MINIMUMS
    )
    require(
        not unsupported,
        f"release executable {executable_name} declares unsupported GNU libc requirements: "
        + ", ".join(unsupported),
    )
    versions = set()
    for requirement in requirements:
        match = NUMERIC_GLIBC_REQUIREMENT.fullmatch(requirement)
        if match is not None:
            versions.add((int(match.group(1)), int(match.group(2))))
        else:
            versions.add(GLIBC_ABI_MINIMUMS[requirement])
    return versions


def required_glibc_versions(executable: pathlib.Path) -> set[tuple[int, int]]:
    probe = subprocess.run(
        ["readelf", "--version-info", "--wide", executable],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    require(probe.returncode == 0, f"readelf rejected release executable {executable.name}: {probe.stderr.strip()}")
    return parse_required_glibc_versions(probe.stdout, executable.name)


def verify_required_glibc_floor(
    executable_name: str,
    versions: set[tuple[int, int]],
    minimum: tuple[int, int],
    minimum_text: str,
) -> None:
    newest = max(versions)
    require(
        newest <= minimum,
        f"{executable_name} requires GLIBC_{newest[0]}.{newest[1]}, "
        f"above the declared {minimum_text} floor",
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=pathlib.Path, nargs="?")
    parser.add_argument("--target", required=True)
    parser.add_argument("--host-only", action="store_true")
    arguments = parser.parse_args()

    require(arguments.target.endswith("-unknown-linux-gnu"), "GNU runtime verification requires a GNU Linux target")
    minimum_text, minimum = runtime_authority()
    actual = host_runtime()
    require(
        actual == minimum_text,
        f"GNU archive qualification must run on the declared glibc {minimum_text} floor, found {actual}",
    )
    if arguments.host_only:
        require(arguments.directory is None, "--host-only does not accept a component directory")
        print(f"verified {arguments.target} qualification host at glibc {minimum_text}")
        return

    require(arguments.directory is not None, "release component directory is required")
    directory = arguments.directory.resolve()
    require(directory.is_dir(), "release component directory is missing")

    for executable, _ in manifest_entries(directory, arguments.target):
        versions = required_glibc_versions(executable)
        verify_required_glibc_floor(
            executable.name, versions, minimum, minimum_text
        )

    print(f"verified every {arguments.target} executable on glibc {minimum_text}")


if __name__ == "__main__":
    main()
