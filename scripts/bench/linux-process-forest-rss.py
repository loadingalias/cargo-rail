#!/usr/bin/env python3
"""Sample aggregate RSS for exact Linux process forests."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time


def process_snapshot() -> dict[int, int]:
    parents: dict[int, int] = {}
    for entry in os.scandir("/proc"):
        if not entry.name.isdecimal():
            continue
        pid = int(entry.name)
        try:
            stat = Path(entry.path, "stat").read_text()
            closing_paren = stat.rfind(")")
            fields = stat[closing_paren + 2 :].split()
            parents[pid] = int(fields[1])
        except (FileNotFoundError, PermissionError, ProcessLookupError, IndexError, ValueError):
            continue
    return parents


def descendants(roots: set[int], parents: dict[int, int]) -> set[int]:
    children: dict[int, list[int]] = {}
    for pid, parent in parents.items():
        children.setdefault(parent, []).append(pid)
    selected = roots & parents.keys()
    pending = list(selected)
    while pending:
        parent = pending.pop()
        for child in children.get(parent, ()):
            if child not in selected:
                selected.add(child)
                pending.append(child)
    return selected


def resident_bytes(pids: set[int], page_size: int) -> int:
    total = 0
    for pid in pids:
        try:
            statm = Path("/proc", str(pid), "statm").read_text().split()
            total += int(statm[1]) * page_size
        except (FileNotFoundError, PermissionError, ProcessLookupError, IndexError, ValueError):
            continue
    return total


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--stop-file", type=Path, required=True)
    parser.add_argument("--interval-ms", type=int, default=10)
    parser.add_argument("roots", type=int, nargs="+")
    args = parser.parse_args()
    if args.interval_ms < 1:
        parser.error("--interval-ms must be positive")

    roots = set(args.roots)
    observed_roots: set[int] = set()
    peak_rss_bytes = 0
    samples = 0
    page_size = os.sysconf("SC_PAGE_SIZE")
    while not args.stop_file.exists():
        parents = process_snapshot()
        observed_roots.update(roots & parents.keys())
        forest = descendants(roots, parents)
        peak_rss_bytes = max(peak_rss_bytes, resident_bytes(forest, page_size))
        samples += 1
        time.sleep(args.interval_ms / 1000)

    payload = {
        "schema_version": 1,
        "available": samples > 0 and observed_roots == roots,
        "scope": "sampled_aggregate_rss_of_command_and_sccache_server_process_forests",
        "interval_ms": args.interval_ms,
        "samples": samples,
        "roots": sorted(roots),
        "observed_roots": sorted(observed_roots),
        "peak_rss_bytes": peak_rss_bytes,
    }
    temporary = args.output.with_name(f".{args.output.name}.tmp")
    temporary.write_text(json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n")
    temporary.replace(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
