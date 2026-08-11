#!/usr/bin/env python3
"""Record and compare untimed per-rustc native-cache coverage."""

from __future__ import annotations

import argparse
import collections
import copy
import hashlib
import json
from pathlib import Path
import re
import shlex
import sys
import tempfile
from typing import Any


EVENT_SCHEMA_VERSION = 1
REPORT_SCHEMA_VERSION = 3
DIRECT_WRAPPER_NAMES = {"cargo-rail-native-rustc-wrapper", "cargo-rail-native-rustc-wrapper.exe"}


def option_values(arguments: list[str], option: str) -> list[str]:
    values: list[str] = []
    index = 0
    inline = f"{option}="
    while index < len(arguments):
        argument = arguments[index]
        if argument == option and index + 1 < len(arguments):
            values.append(arguments[index + 1])
            index += 2
        elif argument.startswith(inline):
            values.append(argument[len(inline) :])
            index += 1
        else:
            index += 1
    return values


def short_option_values(arguments: list[str], option: str) -> list[str]:
    values: list[str] = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == option and index + 1 < len(arguments):
            values.append(arguments[index + 1])
            index += 2
        elif argument.startswith(option) and argument != option:
            values.append(argument[len(option) :])
            index += 1
        else:
            index += 1
    return values


def source_argument(arguments: list[str]) -> str | None:
    for argument in arguments:
        if argument == "-" or argument.endswith(".rs"):
            return argument
    return None


def package_hint(source: str | None, crate_name: str | None) -> str:
    if source is None:
        return crate_name or "compiler-request"
    normalized = source.replace("\\", "/")
    registry = normalized.split("/registry/src/", 1)
    if len(registry) == 2:
        parts = registry[1].split("/")
        if len(parts) >= 2:
            return parts[1]
    crates = normalized.split("/crates/", 1)
    if len(crates) == 2 and crates[1]:
        return crates[1].split("/", 1)[0]
    if normalized.startswith("crates/"):
        return normalized.split("/", 2)[1]
    path = Path(source)
    if path.name == "build.rs" and path.parent.name:
        return path.parent.name
    return crate_name or path.parent.name or path.name


def codegen_options(arguments: list[str]) -> list[str]:
    ignored = {"metadata", "extra-filename", "incremental"}
    selected = []
    for value in short_option_values(arguments, "-C"):
        name = value.split("=", 1)[0]
        if name not in ignored:
            selected.append(value)
    return sorted(selected)


def extern_names(arguments: list[str]) -> list[str]:
    names = []
    for value in option_values(arguments, "--extern"):
        name = value.split("=", 1)[0]
        if name.startswith("priv:"):
            name = name[5:]
        names.append(name)
    return sorted(names)


def native_inputs(arguments: list[str]) -> tuple[list[str], list[str]]:
    libraries = sorted(short_option_values(arguments, "-l"))
    searches = []
    for value in short_option_values(arguments, "-L"):
        kind = value.split("=", 1)[0] if "=" in value else "all"
        if kind != "dependency":
            searches.append(kind)
    return libraries, sorted(searches)


def canonical_action(arguments: list[str]) -> dict[str, Any]:
    crate_name = next(iter(option_values(arguments, "--crate-name")), None)
    crate_types = sorted(
        crate_type
        for value in option_values(arguments, "--crate-type")
        for crate_type in value.split(",")
        if crate_type
    )
    emits = sorted(
        emit.split("=", 1)[0]
        for value in option_values(arguments, "--emit")
        for emit in value.split(",")
        if emit
    )
    cfg = sorted(option_values(arguments, "--cfg"))
    features = sorted(
        value[len('feature="') : -1]
        for value in cfg
        if value.startswith('feature="') and value.endswith('"')
    )
    libraries, native_searches = native_inputs(arguments)
    capabilities = []
    externs = extern_names(arguments)
    if any(name.endswith(("_derive", "_macros", "_macro")) for name in externs):
        capabilities.append("possible_proc_macro_consumer")
    if libraries or native_searches:
        capabilities.append("native_link_consumer")
    source = source_argument(arguments)
    if crate_name is None:
        action_class = "compiler_request"
    elif "proc-macro" in crate_types:
        action_class = "proc_macro_producer"
    elif "bin" in crate_types and crate_name == "build_script_build":
        action_class = "build_script"
    elif "bin" in crate_types:
        action_class = "binary"
    else:
        action_class = "rust_library"
    return {
        "package_hint": package_hint(source, crate_name),
        "crate_name": crate_name,
        "crate_types": crate_types,
        "action_class": action_class,
        "capabilities": capabilities,
        "source_kind": "stdin" if source == "-" else Path(source).name if source else None,
        "edition": next(iter(option_values(arguments, "--edition")), None),
        "target": next(iter(option_values(arguments, "--target")), "host"),
        "test": "--test" in arguments,
        "emit": emits,
        "features": features,
        "cfg": cfg,
        "codegen": codegen_options(arguments),
        "unstable": sorted(short_option_values(arguments, "-Z")),
        "externs": externs,
        "native_libraries": libraries,
        "native_search_kinds": native_searches,
    }


def action_id(action: dict[str, Any]) -> str:
    encoded = json.dumps(action, sort_keys=True, separators=(",", ":")).encode()
    return f"coverage-action-v1:sha256:{hashlib.sha256(encoded).hexdigest()}"


def output_role(path: Path) -> str:
    suffix = path.suffix.lower()
    return {
        ".d": "dep_info",
        ".rmeta": "metadata",
        ".rlib": "rlib",
        ".so": "linked_library",
        ".dylib": "linked_library",
        ".dll": "linked_library",
        ".lib": "linked_library",
        ".a": "linked_library",
        ".o": "object",
        ".obj": "object",
        ".dwo": "split_debug",
        ".pdb": "debug_database",
    }.get(suffix, "linked_output")


def observed_outputs(arguments: list[str], cwd: Path) -> list[dict[str, Any]]:
    out_dirs = option_values(arguments, "--out-dir")
    if not out_dirs:
        return []
    out_dir = Path(out_dirs[-1])
    if not out_dir.is_absolute():
        out_dir = cwd / out_dir
    crate_name = next(iter(option_values(arguments, "--crate-name")), "")
    extra = ""
    for value in short_option_values(arguments, "-C"):
        if value.startswith("extra-filename="):
            extra = value.split("=", 1)[1]
    try:
        candidates = list(out_dir.iterdir())
    except OSError:
        return []
    # Rustc owns only its exact crate output stems. ThinLTO linkers can leave
    # downstream scratch objects in the same directory whose names embed an
    # upstream metadata hash; substring matching would falsely charge those
    # objects to the upstream compile action.
    output_stems = {f"{crate_name}{extra}", f"lib{crate_name}{extra}"}
    outputs = []
    for path in candidates:
        name = path.name
        selected = bool(crate_name) and any(
            name == stem or name.startswith(f"{stem}.") for stem in output_stems
        )
        if not selected:
            continue
        try:
            if not path.is_file() or path.is_symlink():
                continue
            size = path.stat().st_size
        except OSError:
            continue
        outputs.append({"name": name, "role": output_role(path), "logical_bytes": size})
    return sorted(outputs, key=lambda output: (output["role"], output["name"]))


def rust_arguments(arguments: list[str]) -> bool:
    return bool(option_values(arguments, "--crate-name")) and source_argument(arguments) is not None


def normalized_event(
    lane: str,
    compiler: str | None,
    arguments: list[str],
    outcome: str,
    reason: str,
    cwd: Path,
) -> dict[str, Any]:
    action = canonical_action(arguments)
    outputs = observed_outputs(arguments, cwd)
    return {
        "schema_version": EVENT_SCHEMA_VERSION,
        "lane": lane,
        "language": "rust" if rust_arguments(arguments) else "other",
        "compiler": compiler,
        "arguments": arguments,
        "action": action,
        "action_id": action_id(action),
        "outcome": outcome,
        "reason": reason,
        "requested_output_roles": action["emit"],
        "outputs": outputs,
        "logical_output_bytes": sum(output["logical_bytes"] for output in outputs),
        "compiler_executed": outcome != "hit",
        "cache_tier": lane if outcome == "hit" else None,
        "restore_verified": lane == "cargo-rail" and outcome == "hit",
    }


def cold_costs(directory: Path) -> list[dict[str, Any]]:
    costs = []
    for argv_path in sorted(directory.glob("unit-*.argv")):
        stem = argv_path.with_suffix("")
        tsv_path = stem.with_suffix(".tsv")
        if not tsv_path.is_file():
            raise ValueError(f"cold compiler timing has no terminal record: {argv_path}")
        command = [
            argument.decode("utf-8", errors="strict")
            for argument in argv_path.read_bytes().split(b"\0")
            if argument
        ]
        if len(command) < 2:
            raise ValueError(f"cold compiler timing has incomplete argv: {argv_path}")
        fields = tsv_path.read_text(encoding="utf-8", errors="strict").rstrip("\n").split("\t")
        if len(fields) < 11:
            raise ValueError(f"cold compiler timing has an incomplete terminal record: {tsv_path}")
        action = canonical_action(command[1:])
        user_seconds = float(fields[8])
        system_seconds = float(fields[9])
        costs.append(
            {
                "action_id": action_id(action),
                "action": action,
                "compiler": command[0],
                "wall_seconds": float(fields[7]),
                "user_seconds": user_seconds,
                "system_seconds": system_seconds,
                "cpu_seconds": user_seconds + system_seconds,
                "exit_code": int(fields[10]),
                "evidence": "serial_cold_rustc_time_v1",
            }
        )
    if not costs:
        raise ValueError(f"cold compiler timing directory is empty: {directory}")
    return costs


def attach_cold_costs(events: list[dict[str, Any]], costs: list[dict[str, Any]]) -> list[dict[str, Any]]:
    attached = copy.deepcopy(events)
    event_indices: collections.defaultdict[str, list[int]] = collections.defaultdict(list)
    costs_by_action: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for index, event in enumerate(attached):
        if event["language"] == "rust":
            event_indices[event["action_id"]].append(index)
    for cost in costs:
        costs_by_action[cost["action_id"]].append(cost)
    for identifier, indices in event_indices.items():
        matching = costs_by_action[identifier]
        if len(matching) != len(indices):
            continue
        for index, cost in zip(indices, matching, strict=True):
            attached[index]["cold_cost"] = cost
    return attached


def sccache_events(log: Path, cwd: Path) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    pending: collections.defaultdict[str, collections.deque[dict[str, Any]]] = collections.defaultdict(
        collections.deque
    )
    ok = re.compile(r"parse_arguments: Ok: (\[.*\])$")
    bypass = re.compile(r"parse_arguments: CannotCache\((.*?)\): (\[.*\])$")
    cache_decision = re.compile(r"\[([^]]+)\]: Cache (hit|miss) in ")
    completed = re.compile(r"(?:\[([^]]+)\]: )?compile result: cache (hit|miss)$")
    failed = re.compile(r"Compilation failed: Output ")
    selected: dict[str, dict[str, Any]] = {}
    last_local_miss: tuple[str, dict[str, Any]] | None = None
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        match = bypass.search(line)
        if match:
            arguments = json.loads(match.group(2))
            events.append(
                normalized_event("sccache", None, arguments, "bypass", match.group(1), cwd)
            )
            continue
        match = ok.search(line)
        if match:
            arguments = json.loads(match.group(1))
            crate_name = next(iter(option_values(arguments, "--crate-name")), "compiler-request")
            pending[crate_name].append(normalized_event("sccache", None, arguments, "pending", "", cwd))
            continue
        match = cache_decision.search(line)
        if match and pending[match.group(1)]:
            event = pending[match.group(1)][-1]
            selected[match.group(1)] = event
            if match.group(2) == "miss":
                last_local_miss = (match.group(1), event)
            continue
        if failed.search(line) and last_local_miss is not None:
            crate_name, event = last_local_miss
            if event in pending[crate_name]:
                pending[crate_name].remove(event)
                event["outcome"] = "failure"
                event["reason"] = "sccache_compiler_failure"
                events.append(event)
            selected.pop(crate_name, None)
            last_local_miss = None
            continue
        match = completed.search(line)
        if match:
            crate_name = match.group(1)
            if crate_name is None:
                available = [name for name, requests in pending.items() if requests]
                crate_name = available[0] if len(available) == 1 else None
            if crate_name is None or not pending[crate_name]:
                continue
            event = selected.pop(crate_name, pending[crate_name][0])
            pending[crate_name].remove(event)
            event["outcome"] = match.group(2)
            event["reason"] = f"sccache_{match.group(2)}"
            events.append(event)
            if last_local_miss == (crate_name, event):
                last_local_miss = None
    for requests in pending.values():
        for event in requests:
            event["outcome"] = "ambiguous"
            event["reason"] = "log_ended_before_correlated_outcome"
            events.append(event)
    return events


def cargo_verbose_invocations(log: Path) -> list[tuple[str, list[str]]]:
    invocations = []
    start = re.compile(r"^\s*Running `(.*)$")
    command_lines: list[str] | None = None
    for line in log.read_text(encoding="utf-8", errors="strict").splitlines():
        if command_lines is None:
            match = start.match(line)
            if match is None:
                continue
            command_lines = [match.group(1)]
        else:
            command_lines.append(line)
        if not line.endswith("`"):
            continue
        candidate = "\n".join(command_lines)[:-1]
        try:
            command = shlex.split(candidate, posix=True)
        except ValueError:
            continue
        command_lines = None
        if not any(name in candidate for name in DIRECT_WRAPPER_NAMES):
            continue
        wrapper_index = next(
            (
                index
                for index, token in enumerate(command)
                if token.replace("\\", "/").rsplit("/", 1)[-1] in DIRECT_WRAPPER_NAMES
            ),
            None,
        )
        if wrapper_index is None or wrapper_index + 1 >= len(command):
            raise ValueError("Cargo verbose output contains an unparseable Cargo-Rail wrapper invocation")
        invocations.append((command[wrapper_index + 1], command[wrapper_index + 2 :]))
    if command_lines is not None and any(name in "\n".join(command_lines) for name in DIRECT_WRAPPER_NAMES):
        raise ValueError("Cargo verbose output ends inside a Cargo-Rail wrapper invocation")
    return invocations


def load_cargo_rail_events(directory: Path, verbose_log: Path, cwd: Path) -> list[dict[str, Any]]:
    def outcome(status: str) -> str:
        return {"bypassed": "bypass", "disabled": "bypass"}.get(status, status)

    detailed: collections.defaultdict[str, collections.deque[dict[str, Any]]] = collections.defaultdict(
        collections.deque
    )
    for path in sorted(directory.glob("event-*.json")):
        with path.open(encoding="utf-8") as source:
            event = json.load(source)
        if event.get("schema_version") != EVENT_SCHEMA_VERSION:
            raise ValueError(f"coverage event has unsupported schema: {path}")
        action = canonical_action(event["arguments"])
        event["action"] = action
        event["action_id"] = action_id(action)
        detailed[event["action_id"]].append(event)

    events = []
    for compiler, arguments in cargo_verbose_invocations(verbose_log):
        action = canonical_action(arguments)
        identifier = action_id(action)
        detail = detailed[identifier].popleft() if detailed[identifier] else None
        event_cwd = cwd if detail is None else Path(detail["current_directory"])
        selected_outcome = "bypass" if detail is None else outcome(detail["status"])
        reason = "before_context_or_cache_inactive" if detail is None else detail["reason"]
        event = normalized_event("cargo-rail", compiler, arguments, selected_outcome, reason, event_cwd)
        if detail is not None:
            for name in (
                "action_key",
                "result_key",
                "remote_base_action_key",
                "bytes_hashed",
                "cache_bytes_read",
            ):
                if name in detail:
                    event[name] = detail[name]
        events.append(event)
    for requests in detailed.values():
        for detail in requests:
            event = normalized_event(
                "cargo-rail",
                detail["compiler"],
                detail["arguments"],
                outcome(detail["status"]),
                detail["reason"],
                Path(detail["current_directory"]),
            )
            events.append(event)
    return events


def outcome_counts(events: list[dict[str, Any]]) -> dict[str, int]:
    return dict(sorted(collections.Counter(event["outcome"] for event in events).items()))


def hit_metrics(events: list[dict[str, Any]]) -> dict[str, Any]:
    hits = [event for event in events if event["language"] == "rust" and event["outcome"] == "hit"]
    cost_gaps = [event["action_id"] for event in hits if "cold_cost" not in event]
    return {
        "verified_actions": len(hits),
        "logical_output_bytes": sum(event["logical_output_bytes"] for event in hits),
        "eliminated_cold_cpu_seconds": sum(event.get("cold_cost", {}).get("cpu_seconds", 0) for event in hits),
        "eliminated_serial_critical_path_seconds": sum(
            event.get("cold_cost", {}).get("wall_seconds", 0) for event in hits
        ),
        "cold_cost_complete": not cost_gaps,
        "cold_cost_gaps": sorted(cost_gaps),
    }


def coverage_report(
    cargo_rail: list[dict[str, Any]],
    sccache: list[dict[str, Any]],
    costs: list[dict[str, Any]],
) -> dict[str, Any]:
    cargo_rail = attach_cold_costs(cargo_rail, costs)
    sccache = attach_cold_costs(sccache, costs)
    cargo_hits = collections.defaultdict(list)
    for event in cargo_rail:
        if event["language"] == "rust" and event["outcome"] == "hit":
            cargo_hits[event["action_id"]].append(event)
    missing = []
    matched = []
    for event in sccache:
        if event["language"] != "rust" or event["outcome"] != "hit":
            continue
        available = cargo_hits[event["action_id"]]
        if available:
            matched.append({"action_id": event["action_id"], "action": event["action"]})
            available.pop()
        else:
            missing.append(
                {
                    "action_id": event["action_id"],
                    "action": event["action"],
                    "requested_output_roles": event["requested_output_roles"],
                    "logical_output_bytes": event.get("logical_output_bytes", 0),
                }
            )
    extra = [
        {"action_id": action, "action": event["action"]}
        for action, events in sorted(cargo_hits.items())
        for event in events
    ]
    ambiguous = [
        event
        for event in [*cargo_rail, *sccache]
        if event["language"] == "rust" and event["outcome"] == "ambiguous"
    ]
    cargo_metrics = hit_metrics(cargo_rail)
    sccache_metrics = hit_metrics(sccache)
    complete_costs = cargo_metrics["cold_cost_complete"] and sccache_metrics["cold_cost_complete"]
    same_and_faster = (
        not missing
        and cargo_metrics["verified_actions"] >= sccache_metrics["verified_actions"]
        and cargo_metrics["logical_output_bytes"] >= sccache_metrics["logical_output_bytes"]
        and cargo_metrics["eliminated_cold_cpu_seconds"] >= sccache_metrics["eliminated_cold_cpu_seconds"]
        and cargo_metrics["eliminated_serial_critical_path_seconds"]
        >= sccache_metrics["eliminated_serial_critical_path_seconds"]
    )
    more_and_faster = all(
        cargo_metrics[name] > sccache_metrics[name]
        for name in (
            "verified_actions",
            "logical_output_bytes",
            "eliminated_cold_cpu_seconds",
            "eliminated_serial_critical_path_seconds",
        )
    )
    passed = not ambiguous and complete_costs and (same_and_faster or more_and_faster)
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "cargo_rail": {
            "rust_requests": sum(event["language"] == "rust" for event in cargo_rail),
            "outcomes": outcome_counts([event for event in cargo_rail if event["language"] == "rust"]),
            "hit_metrics": cargo_metrics,
            "actions": [event for event in cargo_rail if event["language"] == "rust"],
        },
        "sccache": {
            "rust_requests": sum(event["language"] == "rust" for event in sccache),
            "other_requests": sum(event["language"] != "rust" for event in sccache),
            "outcomes": outcome_counts([event for event in sccache if event["language"] == "rust"]),
            "hit_metrics": sccache_metrics,
            "actions": [event for event in sccache if event["language"] == "rust"],
        },
        "coverage_gate": {
            "passed": passed,
            "route": "same_and_faster" if passed and same_and_faster else "more_and_faster" if passed else None,
            "same_and_faster": same_and_faster and complete_costs and not ambiguous,
            "more_and_faster": more_and_faster and complete_costs and not ambiguous,
            "matched_sccache_hits": len(matched),
            "missing_sccache_hits": missing,
            "extra_cargo_rail_hits": extra,
            "ambiguous_events": ambiguous,
            "cost_evidence": {
                "model": "one serial cold Cargo execution with per-rustc wall and child CPU time",
                "complete": complete_costs,
                "records": len(costs),
            },
        },
    }


def report_command(arguments: argparse.Namespace) -> int:
    cargo_rail = load_cargo_rail_events(
        arguments.cargo_rail_events,
        arguments.cargo_rail_verbose,
        arguments.cargo_rail_root,
    )
    sccache = sccache_events(arguments.sccache_log, arguments.sccache_root)
    costs = cold_costs(arguments.cold_timings)
    report = coverage_report(cargo_rail, sccache, costs)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    with arguments.output.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, sort_keys=True)
        output.write("\n")
    print(json.dumps(report["coverage_gate"], indent=2, sort_keys=True))
    return 0 if report["coverage_gate"]["passed"] else 1


def self_test_command(_arguments: argparse.Namespace) -> int:
    def event(identifier: str, outcome: str, logical_bytes: int, cpu: float, wall: float) -> dict[str, Any]:
        action = {"crate_name": identifier}
        return {
            "schema_version": EVENT_SCHEMA_VERSION,
            "lane": "synthetic",
            "language": "rust",
            "action": action,
            "action_id": identifier,
            "outcome": outcome,
            "requested_output_roles": ["metadata"],
            "logical_output_bytes": logical_bytes,
            "cold_cost": {"cpu_seconds": cpu, "wall_seconds": wall},
        }

    costs = [
        {"action_id": identifier, "cpu_seconds": cpu, "wall_seconds": wall}
        for identifier, cpu, wall in [
            ("shared", 1.0, 1.0),
            ("cargo-extra", 3.0, 4.0),
            ("cargo-extra-2", 1.0, 1.0),
            ("sccache-unsafe", 0.5, 0.5),
        ]
    ]
    cargo = [
        event("shared", "hit", 10, 1.0, 1.0),
        event("cargo-extra", "hit", 50, 3.0, 4.0),
        event("cargo-extra-2", "hit", 5, 1.0, 1.0),
    ]
    sccache = [event("shared", "hit", 10, 1.0, 1.0), event("sccache-unsafe", "hit", 5, 0.5, 0.5)]
    # coverage_report attaches costs from the canonical cost input; remove the
    # synthetic inline copies so this exercises the real completeness path.
    for selected in [*cargo, *sccache]:
        selected.pop("cold_cost")
    report = coverage_report(cargo, sccache, costs)
    assert report["coverage_gate"]["passed"]
    assert report["coverage_gate"]["route"] == "more_and_faster"
    incomplete = coverage_report(cargo, sccache, costs[:-1])
    assert not incomplete["coverage_gate"]["passed"]
    assert not incomplete["coverage_gate"]["cost_evidence"]["complete"]
    with tempfile.TemporaryDirectory(prefix="cargo-rail-coverage-") as temporary:
        output_dir = Path(temporary)
        (output_dir / "libdependency-deadbeef.rlib").write_bytes(b"library")
        (output_dir / "dependency-deadbeef.d").write_bytes(b"dep-info")
        (output_dir / "final-binary-deadbeef.dependency-deadbeef.module.rcgu.o").write_bytes(
            b"downstream ThinLTO scratch"
        )
        outputs = observed_outputs(
            [
                "--crate-name",
                "dependency",
                "dependency.rs",
                "--out-dir",
                temporary,
                "-C",
                "extra-filename=-deadbeef",
            ],
            output_dir,
        )
        assert [output["name"] for output in outputs] == [
            "dependency-deadbeef.d",
            "libdependency-deadbeef.rlib",
        ]
    print("native-cache coverage ledger self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    report = subparsers.add_parser("report")
    report.add_argument("--cargo-rail-events", type=Path, required=True)
    report.add_argument("--cargo-rail-verbose", type=Path, required=True)
    report.add_argument("--cargo-rail-root", type=Path, required=True)
    report.add_argument("--sccache-log", type=Path, required=True)
    report.add_argument("--sccache-root", type=Path, required=True)
    report.add_argument("--cold-timings", type=Path, required=True)
    report.add_argument("--output", type=Path, required=True)
    report.set_defaults(handler=report_command)
    self_test = subparsers.add_parser("self-test")
    self_test.set_defaults(handler=self_test_command)
    arguments = parser.parse_args()
    return arguments.handler(arguments)


if __name__ == "__main__":
    sys.exit(main())
