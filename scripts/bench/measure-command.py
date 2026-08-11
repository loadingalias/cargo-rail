#!/usr/bin/env python3
"""Run one benchmark command and retain exact elapsed/process evidence."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

try:
    import resource
except ImportError:  # pragma: no cover - Windows benchmark hosts
    resource = None


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("--cwd", required=True)
    value.add_argument("--stdout", required=True)
    value.add_argument("--stderr", required=True)
    value.add_argument("--output", required=True)
    value.add_argument("--env", action="append", default=[])
    value.add_argument("--unset", action="append", default=[])
    value.add_argument("command", nargs=argparse.REMAINDER)
    return value


def main() -> int:
    arguments = parser().parse_args()
    command = arguments.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser().error("a command is required after --")

    environment = os.environ.copy()
    for name in arguments.unset:
        environment.pop(name, None)
    for assignment in arguments.env:
        name, separator, value = assignment.partition("=")
        if not separator or not name:
            parser().error(f"invalid --env assignment: {assignment!r}")
        environment[name] = value

    before = resource.getrusage(resource.RUSAGE_CHILDREN) if resource is not None else None
    started = time.perf_counter_ns()
    with Path(arguments.stdout).open("wb") as stdout, Path(arguments.stderr).open("wb") as stderr:
        completed = subprocess.run(command, cwd=arguments.cwd, env=environment, stdout=stdout, stderr=stderr)
    finished = time.perf_counter_ns()
    after = resource.getrusage(resource.RUSAGE_CHILDREN) if resource is not None else None

    evidence = {
        "schema_version": 1,
        "argv": command,
        "cwd": str(Path(arguments.cwd).resolve()),
        "elapsed_seconds": (finished - started) / 1_000_000_000,
        "user_seconds": None if before is None else after.ru_utime - before.ru_utime,
        "system_seconds": None if before is None else after.ru_stime - before.ru_stime,
        "max_rss_observed": None if after is None else after.ru_maxrss,
        "exit_code": completed.returncode,
    }
    Path(arguments.output).write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
