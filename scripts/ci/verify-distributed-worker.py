#!/usr/bin/env python3
"""Smoke-test the distributed worker against a selected Rust compiler."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
from typing import Any


class VerificationError(RuntimeError):
    """The worker or selected compiler violated its command contract."""


def run(program: str, *arguments: str) -> bytes:
    completed = subprocess.run(
        [program, *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        stderr = completed.stderr.decode("utf-8", errors="replace").strip()
        raise VerificationError(
            f"{pathlib.Path(program).name} {' '.join(arguments)} failed "
            f"with exit code {completed.returncode}: {stderr or '<no stderr>'}"
        )
    if completed.stderr:
        stderr = completed.stderr.decode("utf-8", errors="replace").strip()
        raise VerificationError(
            f"{pathlib.Path(program).name} {' '.join(arguments)} contaminated stderr: {stderr}"
        )
    return completed.stdout


def text(program: str, *arguments: str) -> str:
    try:
        return run(program, *arguments).decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise VerificationError(
            f"{pathlib.Path(program).name} {' '.join(arguments)} returned non-UTF-8 output"
        ) from error


def capability(worker: str, rustc: str) -> dict[str, Any]:
    encoded = text(worker, "capability", rustc)
    try:
        value = json.loads(encoded)
    except json.JSONDecodeError as error:
        raise VerificationError("distributed worker capability output is not JSON") from error
    if not isinstance(value, dict):
        raise VerificationError("distributed worker capability output is not an object")
    return value


def require(condition: bool, message: str) -> None:
    if not condition:
        raise VerificationError(message)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--worker", required=True)
    parser.add_argument("--rustc", default="rustc")
    args = parser.parse_args()

    try:
        protocol_text = text(args.worker, "protocol-version")
        require(protocol_text.isascii() and protocol_text.isdigit(), "worker protocol version is not an integer")
        protocol = int(protocol_text)
        require(protocol > 0, "worker protocol version must be positive")

        selected = capability(args.worker, args.rustc)
        require(selected.get("protocol_version") == protocol, "worker commands report different protocol versions")
        require(isinstance(selected.get("host_target"), str) and bool(selected["host_target"]), "capability has no host target")
        require(
            isinstance(selected.get("capability_id"), str)
            and selected["capability_id"].startswith("worker-capability-v")
            and ":sha256:" in selected["capability_id"],
            "capability has no versioned content identity",
        )

        verbose = text(args.rustc, "-vV")
        host = next((line.removeprefix("host: ") for line in verbose.splitlines() if line.startswith("host: ")), "")
        require(host == selected["host_target"], "capability host does not match the selected compiler")

        sysroot = pathlib.Path(text(args.rustc, "--print=sysroot"))
        implementation = sysroot / "bin" / ("rustc.exe" if os.name == "nt" else "rustc")
        require(implementation.is_file(), f"sysroot compiler is missing: {implementation}")
        exact = capability(args.worker, str(implementation))
        require(selected == exact, "selected compiler and exact sysroot compiler capabilities differ")
    except (OSError, VerificationError) as error:
        print(f"distributed-worker-smoke: {error}", file=sys.stderr)
        return 1

    print(f"distributed worker protocol {protocol} verified for {selected['host_target']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
