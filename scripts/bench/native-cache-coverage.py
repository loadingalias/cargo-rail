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


EVENT_SCHEMA_VERSION = 8
REPORT_SCHEMA_VERSION = 7
INVENTORY_SCHEMA_VERSION = 5
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
    components = [component for component in normalized.split("/") if component]
    if components and components[-1] == "build.rs" and len(components) >= 2:
        return components[-2]
    return crate_name or portable_basename(source) or "compiler-request"


def portable_basename(path: str) -> str | None:
    return next((component for component in reversed(path.replace("\\", "/").split("/")) if component), None)


def configured_path(value: str) -> bool:
    return "/" in value or "\\" in value


def cfg_shape(value: str) -> str:
    name, separator, configured = value.partition("=")
    return f"{name}=<path>" if separator and configured_path(configured) else value


def tool_shape(option: str, program: str) -> str:
    name = portable_basename(program)
    return f"{option}={name}" if name else f"{option}=<external-tool>"


def compiler_option_shape(value: str) -> str:
    name, separator, configured = value.partition("=")
    if name in {"linker", "dlltool"} and separator:
        return tool_shape(name, configured)
    if name in {"link-arg", "link-args"}:
        return f"{name}=<opaque>"
    if separator and configured_path(configured):
        return f"{name}=<path>"
    return value


def unstable_option_shape(value: str) -> str:
    name, separator, configured = value.partition("=")
    if name == "codegen-backend" and separator and configured_path(configured):
        return tool_shape(name, configured)
    if separator and configured_path(configured):
        return f"{name}=<path>"
    return value


def target_shape(value: str) -> str:
    if value != "host" and (configured_path(value) or value.endswith(".json")):
        name = portable_basename(value)
        return f"custom-target:{name}" if name else "custom-target"
    return value


def codegen_options(arguments: list[str]) -> list[str]:
    ignored = {"metadata", "extra-filename", "incremental"}
    selected = []
    for value in short_option_values(arguments, "-C"):
        name = value.split("=", 1)[0]
        if name not in ignored:
            selected.append(compiler_option_shape(value))
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


def compiler_driver(program: str | None) -> str:
    name = Path(program or "rustc").stem.lower()
    return {
        "clippy-driver": "clippy",
        "rustc": "rustc",
        "rustdoc": "rustdoc",
    }.get(name, "other")


def canonical_action(arguments: list[str], compiler: str | None = "rustc") -> dict[str, Any]:
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
    cfg_values = option_values(arguments, "--cfg")
    cfg = sorted(cfg_shape(value) for value in cfg_values)
    features = sorted(
        value[len('feature="') : -1]
        for value in cfg_values
        if value.startswith('feature="') and value.endswith('"')
    )
    libraries, native_searches = native_inputs(arguments)
    capabilities = []
    externs = extern_names(arguments)
    extern_artifact_suffixes = {
        Path(value.split("=", 1)[1]).suffix.lower()
        for value in option_values(arguments, "--extern")
        if "=" in value
    }
    if extern_artifact_suffixes & {".dll", ".dylib", ".so"}:
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
    elif "--test" in arguments:
        action_class = "test"
    elif "bin" in crate_types:
        action_class = "binary"
    elif len(crate_types) != 1:
        action_class = "mixed_crate_types"
    elif "staticlib" in crate_types:
        action_class = "static_library"
    elif "cdylib" in crate_types:
        action_class = "c_dynamic_library"
    elif "dylib" in crate_types:
        action_class = "rust_dynamic_library"
    else:
        action_class = "rust_library"
    return {
        "action_class": action_class,
        "capabilities": capabilities,
        "cfg": cfg,
        "codegen": codegen_options(arguments),
        "crate_name": crate_name,
        "crate_types": crate_types,
        "driver": compiler_driver(compiler),
        "edition": next(iter(option_values(arguments, "--edition")), None),
        "emit": emits,
        "externs": externs,
        "features": features,
        "native_libraries": libraries,
        "native_search_kinds": native_searches,
        "package_hint": package_hint(source, crate_name),
        "schema_version": 3,
        "source_name": "stdin" if source == "-" else portable_basename(source) if source else None,
        "target": target_shape(next(iter(option_values(arguments, "--target")), "host")),
        "test": "--test" in arguments,
        "unstable": sorted(unstable_option_shape(value) for value in short_option_values(arguments, "-Z")),
    }


def action_id(action: dict[str, Any]) -> str:
    encoded = json.dumps(action, sort_keys=True, separators=(",", ":")).encode()
    return f"coverage-action-v3:sha256:{hashlib.sha256(encoded).hexdigest()}"


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
    action = canonical_action(arguments, compiler)
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
        action = canonical_action(command[1:], command[0])
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


def cargo_verbose_commands(log: Path) -> list[list[str]]:
    commands = []
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
        except ValueError as error:
            raise ValueError("Cargo verbose output contains an invalid command") from error
        command_lines = None
        commands.append(command)
    if command_lines is not None:
        raise ValueError("Cargo verbose output ends inside a command invocation")
    return commands


def cargo_verbose_invocations(log: Path) -> list[tuple[str, list[str]]]:
    invocations = []
    for command in cargo_verbose_commands(log):
        wrapper_index = next(
            (
                index
                for index, token in enumerate(command)
                if token.replace("\\", "/").rsplit("/", 1)[-1] in DIRECT_WRAPPER_NAMES
            ),
            None,
        )
        if wrapper_index is None:
            continue
        if wrapper_index + 1 >= len(command):
            raise ValueError("Cargo verbose output contains an unparseable Cargo-Rail wrapper invocation")
        invocations.append((command[wrapper_index + 1], command[wrapper_index + 2 :]))
    return invocations


def split_verbose_command(command: list[str]) -> tuple[dict[str, str], str, list[str]]:
    environment: dict[str, str] = {}
    index = 0
    assignment = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
    while index < len(command) and assignment.match(command[index]):
        name, value = command[index].split("=", 1)
        if name in environment:
            raise ValueError(f"Cargo verbose command repeats environment field {name}")
        environment[name] = value
        index += 1
    if index >= len(command):
        raise ValueError("Cargo verbose command has no program")
    return environment, command[index], command[index + 1 :]


def cargo_messages(path: Path) -> list[dict[str, Any]]:
    messages = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8", errors="strict").splitlines(), 1):
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"Cargo message line {line_number} is malformed: {error}") from error
        if isinstance(message, dict) and isinstance(message.get("reason"), str):
            messages.append(message)
    return messages


def cargo_package_label(package_id: str, package_hint_value: str) -> str:
    fragment = package_id.rsplit("#", 1)[-1]
    if "@" in fragment:
        name, version = fragment.rsplit("@", 1)
        if name and version:
            return f"{name} {version}"
    if fragment and fragment != package_id:
        return f"{package_hint_value} {fragment}"
    return package_hint_value


def portable_path(path: str, root: Path) -> str:
    selected = Path(path)
    try:
        return selected.resolve(strict=False).relative_to(root.resolve(strict=True)).as_posix()
    except (OSError, ValueError):
        return selected.name


def portable_value(value: str, root: Path) -> str:
    roots = {str(root), str(root.resolve(strict=True))}
    spellings = {
        spelling
        for root_spelling in roots
        for spelling in (root_spelling, root_spelling.replace("\\", "/"), root_spelling.replace("/", "\\"))
    }
    portable = value
    for spelling in sorted(spellings, key=len, reverse=True):
        portable = portable.replace(spelling, "<workspace>")
    return portable


def generated_outputs(directory: Path) -> list[dict[str, Any]]:
    if not directory.is_dir() or directory.is_symlink():
        return []
    outputs = []
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            try:
                symlink_target = str(path.readlink())
            except OSError as error:
                raise ValueError(f"build-script output symlink is unreadable: {path}") from error
            outputs.append(
                {
                    "name": path.relative_to(directory).as_posix(),
                    "role": "generated_symlink",
                    "logical_bytes": 0,
                    "symlink_target": symlink_target,
                }
            )
            continue
        if not path.is_file():
            continue
        observation = regular_file_content(path)
        if observation is None:
            raise ValueError(f"build-script output bytes are unavailable: {path}")
        size, digest = observation
        outputs.append(
            {
                "name": path.relative_to(directory).as_posix(),
                "role": "generated_file",
                "logical_bytes": size,
                "content_digest": digest,
            }
        )
        if len(outputs) > 100_000:
            raise ValueError("build-script generated output inventory exceeds its entry bound")
    return outputs


def regular_file_content(path: Path) -> tuple[int, str] | None:
    try:
        if not path.is_file() or path.is_symlink():
            return None
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                size += len(chunk)
                digest.update(chunk)
    except OSError:
        return None
    return size, f"sha256:{digest.hexdigest()}"


def content_observation(path: Path, root: Path) -> dict[str, Any] | None:
    observation = regular_file_content(path)
    if observation is None:
        return None
    size, digest = observation
    return {
        "path": portable_path(str(path), root),
        "logical_bytes": size,
        "content_digest": digest,
    }


def root_relative_or_absolute(path: Path, root: Path) -> str:
    try:
        return path.resolve(strict=False).relative_to(root.resolve(strict=True)).as_posix()
    except (OSError, ValueError):
        return str(path.resolve(strict=False))


def resolve_native_program(
    program: str,
    _environment: dict[str, str | None],
    cwd: Path,
) -> Path | None:
    selected = Path(program)
    if selected.is_absolute():
        candidate = selected
    elif selected.parent != Path("."):
        candidate = cwd / selected
    else:
        return None
    try:
        resolved = candidate.resolve(strict=True)
        return resolved if resolved.is_file() else None
    except OSError:
        return None


def split_native_command(command: str) -> tuple[dict[str, str | None], str, list[str]]:
    try:
        tokens = shlex.split(command, posix=True)
    except ValueError as error:
        raise ValueError("cc debug output contains an invalid native command") from error
    if not tokens:
        raise ValueError("cc debug output contains an empty native command")
    environment: dict[str, str | None] = {}
    index = 0
    assignment = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
    while index < len(tokens) and assignment.match(tokens[index]):
        name, value = tokens[index].split("=", 1)
        environment[name] = value
        index += 1
    if index < len(tokens) and Path(tokens[index]).name == "env":
        index += 1
        while index < len(tokens):
            if tokens[index] == "-u" and index + 1 < len(tokens):
                environment[tokens[index + 1]] = None
                index += 2
            elif assignment.match(tokens[index]):
                name, value = tokens[index].split("=", 1)
                environment[name] = value
                index += 1
            else:
                break
    if index >= len(tokens):
        raise ValueError("cc debug output native command has no program")
    return environment, tokens[index], tokens[index + 1 :]


def native_output_path(operation_class: str, arguments: list[str], cwd: Path) -> Path | None:
    if "-o" in arguments:
        index = len(arguments) - 1 - arguments[::-1].index("-o")
        if index + 1 < len(arguments):
            output = Path(arguments[index + 1])
            return output if output.is_absolute() else cwd / output
    if operation_class == "archive":
        for argument in arguments:
            candidate = Path(argument)
            if candidate.suffix.lower() in {".a", ".lib"}:
                return candidate if candidate.is_absolute() else cwd / candidate
    return None


def native_input_paths(operation_class: str, arguments: list[str], cwd: Path) -> list[Path]:
    suffixes = {
        "native_compile": {".c", ".cc", ".cpp", ".cxx"},
        "assembly": {".s", ".asm"},
        "preprocessed_assembly": {".S"},
        "archive": {".o", ".obj", ".a", ".lib"},
        "native_tool_probe": {".c", ".cc", ".cpp", ".cxx", ".s", ".S", ".asm"},
    }[operation_class]
    inputs = []
    output = native_output_path(operation_class, arguments, cwd)
    for argument in arguments:
        candidate = Path(argument)
        if candidate.suffix not in suffixes and candidate.suffix.lower() not in suffixes:
            continue
        path = candidate if candidate.is_absolute() else cwd / candidate
        if output is None or path != output:
            inputs.append(path)
    return inputs


def classify_native_operation(program: str, arguments: list[str]) -> str:
    name = Path(program).stem.lower()
    if name in {"ar", "gcc-ar", "llvm-ar", "lib", "libtool"}:
        return "archive"
    if "-c" in arguments or "/c" in arguments:
        sources = [Path(argument).suffix for argument in arguments]
        if ".S" in sources:
            return "preprocessed_assembly"
        if any(suffix.lower() in {".s", ".asm"} for suffix in sources):
            return "assembly"
        return "native_compile"
    return "native_tool_probe"


def native_child_inventory(
    messages_path: Path,
    verbose_path: Path,
    root: Path,
    build_nodes: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    running = re.compile(r"^\[([^]]+)] running: (.*)$")
    exited = re.compile(r"^\[([^]]+)] exit status: (-?[0-9]+)$")
    by_package: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    for node in build_nodes:
        by_package[node["cargo_package_label"]].append(node)
    completed: list[dict[str, Any]] = []
    gaps: list[dict[str, Any]] = []
    transcripts = (
        verbose_path.read_text(encoding="utf-8", errors="strict"),
        messages_path.read_text(encoding="utf-8", errors="strict"),
    )
    for transcript in transcripts:
        pending: dict[str, dict[str, Any]] = {}
        for line in transcript.splitlines():
            match = running.match(line)
            if match:
                label, command = match.groups()
                package = label
                if package in pending:
                    incomplete = pending.pop(package)
                    incomplete["exit_code"] = None
                    completed.append(incomplete)
                environment, program, arguments = split_native_command(command)
                pending[package] = {
                    "package": package,
                    "environment": environment,
                    "program": program,
                    "arguments": arguments,
                }
                continue
            match = exited.match(line)
            if match:
                label, status = match.groups()
                package = label
                operation = pending.pop(package, None)
                if operation is None:
                    gaps.append(
                        {
                            "kind": "rust_required_native_operation",
                            "package": package,
                            "reason": "native_child_launch_unavailable",
                        }
                    )
                    continue
                operation["exit_code"] = int(status)
                completed.append(operation)
        for package in sorted(pending):
            incomplete = pending[package]
            incomplete["exit_code"] = None
            completed.append(incomplete)

    last_successful_output: dict[str, int] = {}
    for index, operation in enumerate(completed):
        parents = by_package[operation["package"]]
        if len(parents) != 1 or operation["exit_code"] != 0:
            continue
        operation_class = classify_native_operation(operation["program"], operation["arguments"])
        cwd = root / parents[0]["working_directory"]
        output = native_output_path(operation_class, operation["arguments"], cwd)
        if output is not None:
            last_successful_output[str(output)] = index

    nodes = []
    edges = []
    identities: collections.Counter[str] = collections.Counter()
    for index, operation in enumerate(completed):
        package = operation["package"]
        parents = by_package[package]
        if len(parents) != 1:
            gaps.append(
                {
                    "kind": "rust_required_native_operation",
                    "package": package,
                    "reason": "native_child_build_script_correlation_ambiguous",
                }
            )
            continue
        parent = parents[0]
        cwd = root / parent["working_directory"]
        program_path = resolve_native_program(operation["program"], operation["environment"], cwd)
        program_contents = None if program_path is None else regular_file_content(program_path)
        operation_class = classify_native_operation(operation["program"], operation["arguments"])
        portable_arguments = [portable_value(argument, root) for argument in operation["arguments"]]
        action = {
            "kind": operation_class,
            "program_name": Path(operation["program"]).name,
            "program_path": (
                None
                if program_path is None
                else root_relative_or_absolute(program_path, root)
            ),
            "program_digest": (
                None
                if program_contents is None
                else program_contents[1]
            ),
            "arguments": portable_arguments,
            "environment": [
                {"name": name, "value": None if value is None else portable_value(value, root)}
                for name, value in sorted(operation["environment"].items())
            ],
            "package_hint": package,
            "host": parent["action"]["host"],
            "target": parent["action"]["target"],
            "schema_version": 2,
        }
        encoded = json.dumps(action, sort_keys=True, separators=(",", ":")).encode()
        action_identity = f"coverage-operation-v2:sha256:{hashlib.sha256(encoded).hexdigest()}"
        occurrence = identities[action_identity]
        identities[action_identity] += 1
        node_id = f"{action_identity}:{occurrence}"
        inputs = []
        unavailable_inputs = []
        for path in native_input_paths(operation_class, operation["arguments"], cwd):
            observation = content_observation(path, root)
            if observation is None:
                unavailable_inputs.append(root_relative_or_absolute(path, root))
            else:
                inputs.append(observation)
        output_path = native_output_path(operation_class, operation["arguments"], cwd)
        declared_output = (
            None
            if output_path is None
            else root_relative_or_absolute(output_path, root)
        )
        output = (
            content_observation(output_path, root)
            if output_path is not None
            and operation["exit_code"] == 0
            and last_successful_output.get(str(output_path)) == index
            else None
        )
        if output_path is None:
            output_observation = "not_declared"
        elif operation["exit_code"] != 0:
            output_observation = "unsuccessful_operation"
        elif last_successful_output.get(str(output_path)) != index:
            output_observation = "superseded_by_later_mutation"
        elif output is None:
            output_observation = "output_bytes_unavailable"
        else:
            output_observation = "observed"
        missing_capabilities = ["inherited_environment", "negative_filesystem_lookups"]
        cold_reasons = [
            "native_child_inherited_environment_unavailable",
            "native_child_negative_lookup_evidence_unavailable",
        ]
        if program_contents is None:
            missing_capabilities.append("program_identity")
            cold_reasons.append("native_child_program_identity_unavailable")
        if unavailable_inputs:
            missing_capabilities.append("input_bytes")
            cold_reasons.append("native_child_input_bytes_unavailable")
        if output_observation == "output_bytes_unavailable":
            missing_capabilities.append("output_bytes")
            cold_reasons.append("native_child_output_bytes_unavailable")
        elif output_observation == "superseded_by_later_mutation":
            missing_capabilities.append("superseded_output_bytes")
            cold_reasons.append("native_child_output_superseded_by_later_mutation")
        elif output_observation == "unsuccessful_operation" and output_path is not None:
            missing_capabilities.append("unsuccessful_output_state")
            cold_reasons.append("native_child_unsuccessful_output_state_unavailable")
        if operation["exit_code"] is None:
            missing_capabilities.append("terminal_status")
            reason = "native_child_terminal_status_unavailable"
        elif operation_class in {"native_compile", "assembly", "preprocessed_assembly"}:
            missing_capabilities.append("compiler_dependency_file")
            reason = f"{operation_class}_dependency_evidence_unavailable"
        elif operation_class == "archive":
            missing_capabilities.append("archive_mutation_transaction")
            reason = "native_archive_mutation_transaction_unavailable"
        else:
            missing_capabilities.append("probe_side_effects")
            reason = "native_tool_probe_side_effect_observation_unavailable"
        cold_reasons.append(reason)
        nodes.append(
            {
                "id": node_id,
                "kind": operation_class,
                "action": action,
                "program": Path(operation["program"]).name,
                "arguments": portable_arguments,
                "working_directory": parent["working_directory"],
                "declared_output": declared_output,
                "inputs": inputs,
                "unavailable_inputs": unavailable_inputs,
                "outputs": [] if output is None else [output],
                "output_observation": output_observation,
                "logical_output_bytes": 0 if output is None else output["logical_bytes"],
                "exit_code": operation["exit_code"],
                "outcome": "cold",
                "reason": reason,
                "cold_reasons": cold_reasons,
                "missing_capabilities": missing_capabilities,
            }
        )
        edges.append(
            {
                "producer": parent["id"],
                "consumer": node_id,
                "role": "build_script_child_process",
                "artifact": Path(operation["program"]).name,
            }
        )
    previous_mutation: dict[str, str] = {}
    for node in nodes:
        output = node["declared_output"]
        if output is None:
            continue
        previous = previous_mutation.get(output)
        if previous is not None:
            edges.append(
                {
                    "producer": previous,
                    "consumer": node["id"],
                    "role": "native_output_attempt_order",
                    "artifact": Path(output).name,
                }
            )
        previous_mutation[output] = node["id"]
    return nodes, edges, gaps


def build_script_inventory(messages_path: Path, verbose_path: Path, root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    messages = cargo_messages(messages_path)
    artifacts: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    executions = []
    for message in messages:
        if message["reason"] == "compiler-artifact":
            target = message.get("target")
            if isinstance(target, dict) and "custom-build" in target.get("kind", []):
                artifacts[message["package_id"]].append(message)
        elif message["reason"] == "build-script-executed":
            executions.append(message)

    commands: collections.defaultdict[str, collections.deque[tuple[dict[str, str], str, list[str]]]] = (
        collections.defaultdict(collections.deque)
    )
    for command in cargo_verbose_commands(verbose_path):
        environment, program, arguments = split_verbose_command(command)
        output_directory = environment.get("OUT_DIR")
        if output_directory is None:
            continue
        program_name = Path(program).stem.replace("_", "-")
        if not program_name.startswith("build-script-"):
            continue
        commands[str(Path(output_directory))].append((environment, program, arguments))

    nodes = []
    gaps = []
    identities: collections.Counter[str] = collections.Counter()
    for execution in executions:
        package_id = execution.get("package_id")
        output_directory = execution.get("out_dir")
        if not isinstance(package_id, str) or not isinstance(output_directory, str):
            raise ValueError("Cargo build-script message lacks its package or output directory")
        candidates = artifacts[package_id]
        if len(candidates) != 1:
            gaps.append(
                {
                    "kind": "build_script_execution",
                    "package": package_id,
                    "reason": "build_script_compiler_artifact_correlation_ambiguous",
                }
            )
            continue
        artifact = candidates[0]
        target = artifact["target"]
        source = target.get("src_path")
        package = package_hint(source if isinstance(source, str) else None, None)
        selected = commands[output_directory].popleft() if commands[output_directory] else None
        if selected is not None:
            environment, program, arguments = selected
            outcome = "cold"
            reason = "build_script_ambient_effect_observation_unavailable"
        else:
            environment = {}
            program = "unknown"
            arguments = []
            outcome = "ambiguous"
            reason = "build_script_execution_launch_unavailable"
            gaps.append(
                {
                    "kind": "build_script_execution",
                    "package": package,
                    "reason": reason,
                }
            )
        action = {
            "kind": "build_script_execution",
            "package_hint": package,
            "features": sorted(artifact.get("features", [])),
            "host": environment.get("HOST", "unknown"),
            "target": environment.get("TARGET", "unknown"),
            "profile": environment.get("PROFILE", "unknown"),
            "opt_level": environment.get("OPT_LEVEL", "unknown"),
            "schema_version": 1,
        }
        encoded = json.dumps(action, sort_keys=True, separators=(",", ":")).encode()
        action_identity = f"coverage-operation-v1:sha256:{hashlib.sha256(encoded).hexdigest()}"
        occurrence = identities[action_identity]
        identities[action_identity] += 1
        node_id = f"{action_identity}:{occurrence}"
        outputs = generated_outputs(Path(output_directory))
        linked_libraries = execution.get("linked_libs", [])
        linked_paths = execution.get("linked_paths", [])
        nodes.append(
            {
                "id": node_id,
                "kind": "build_script_execution",
                "action": action,
                "program": Path(program).name,
                "arguments": arguments,
                "working_directory": portable_path(environment.get("CARGO_MANIFEST_DIR", str(root)), root),
                "compiled_executables": sorted(Path(path).name for path in artifact.get("filenames", [])),
                "cargo_package_label": cargo_package_label(package_id, package),
                "linked_libraries": sorted(linked_libraries),
                "linked_paths": sorted(Path(path).name for path in linked_paths),
                "outputs": outputs,
                "logical_output_bytes": sum(output["logical_bytes"] for output in outputs),
                "exit_code": 0 if selected is not None else None,
                "outcome": outcome,
                "reason": reason,
                "missing_capabilities": (
                    [
                        "ambient_environment_reads",
                        "ambient_filesystem_reads",
                        "child_processes",
                        "clock",
                        "network",
                        "persistent_output_state",
                        "randomness",
                    ]
                ),
            }
        )
    for output_directory, pending in commands.items():
        for _environment, program, _arguments in pending:
            gaps.append(
                {
                    "kind": "build_script_execution",
                    "program": Path(program).name,
                    "output_directory": Path(output_directory).name,
                    "reason": "build_script_cargo_message_correlation_unavailable",
                }
            )
    return nodes, gaps


def extern_artifacts(arguments: list[str]) -> list[tuple[str, str]]:
    artifacts = []
    for value in option_values(arguments, "--extern"):
        if "=" not in value:
            continue
        name, path = value.split("=", 1)
        artifacts.append((name.removeprefix("priv:"), Path(path).name))
    return sorted(artifacts)


def operation_inventory(
    compiler_events: list[dict[str, Any]],
    messages_path: Path,
    verbose_path: Path,
    root: Path,
) -> dict[str, Any]:
    ordered_events = sorted(
        compiler_events,
        key=lambda event: (
            event["action_id"],
            tuple((output["role"], output["name"]) for output in event["outputs"]),
            event["outcome"],
            event["reason"],
        ),
    )
    occurrences: collections.Counter[str] = collections.Counter()
    compiler_nodes = []
    producers: collections.defaultdict[str, list[str]] = collections.defaultdict(list)
    build_script_producers: collections.defaultdict[str, list[str]] = collections.defaultdict(list)
    for event in ordered_events:
        occurrence = occurrences[event["action_id"]]
        occurrences[event["action_id"]] += 1
        node_id = f'{event["action_id"]}:{occurrence}'
        node = {
            "id": node_id,
            "kind": "rust_compiler",
            "action_id": event["action_id"],
            "action": event["action"],
            "program": Path(event["compiler"]).name if event.get("compiler") else None,
            "arguments": event["arguments"],
            "working_directory": portable_path(event.get("current_directory", str(root)), root),
            "outputs": event["outputs"],
            "logical_output_bytes": event["logical_output_bytes"],
            "outcome": event["outcome"],
            "reason": event["reason"],
        }
        compiler_nodes.append(node)
        if event["action"]["action_class"] == "build_script":
            build_script_producers[event["action"]["package_hint"]].append(node_id)
        for output in event["outputs"]:
            producers[output["name"]].append(node_id)

    build_nodes, gaps = build_script_inventory(messages_path, verbose_path, root)
    native_nodes, native_edges, native_gaps = native_child_inventory(messages_path, verbose_path, root, build_nodes)
    gaps.extend(native_gaps)
    compiler_nodes_by_id = {node["id"]: node for node in compiler_nodes}
    edges = native_edges
    for node, event in zip(compiler_nodes, ordered_events, strict=True):
        for extern_name, artifact in extern_artifacts(event["arguments"]):
            matches = producers[artifact]
            if len(matches) == 1:
                producer = compiler_nodes_by_id[matches[0]]
                edges.append(
                    {
                        "producer": matches[0],
                        "consumer": node["id"],
                        "role": (
                            "proc_macro_dependency"
                            if producer["action"]["action_class"] == "proc_macro_producer"
                            else "rust_dependency"
                        ),
                        "name": extern_name,
                        "artifact": artifact,
                    }
                )
            else:
                gaps.append(
                    {
                        "kind": "rust_dependency",
                        "consumer": node["id"],
                        "artifact": artifact,
                        "reason": "compiler_dependency_correlation_ambiguous",
                    }
                )
    native_output_producers: collections.defaultdict[str, list[str]] = collections.defaultdict(list)
    for node in native_nodes:
        for output in node["outputs"]:
            native_output_producers[output["path"]].append(node["id"])
    for node in native_nodes:
        for native_input in node["inputs"]:
            matches = [
                producer
                for producer in native_output_producers[native_input["path"]]
                if producer != node["id"]
            ]
            if len(matches) == 1:
                edges.append(
                    {
                        "producer": matches[0],
                        "consumer": node["id"],
                        "role": "native_artifact",
                        "artifact": Path(native_input["path"]).name,
                    }
                )
            elif len(matches) > 1:
                gaps.append(
                    {
                        "kind": "rust_required_native_operation",
                        "consumer": node["id"],
                        "artifact": native_input["path"],
                        "reason": "native_child_artifact_correlation_ambiguous",
                    }
                )
    for node, event in zip(compiler_nodes, ordered_events, strict=True):
        searches = []
        for value in short_option_values(event["arguments"], "-L"):
            kind, separator, path = value.partition("=")
            if separator and kind == "native":
                selected = Path(path)
                searches.append(selected if selected.is_absolute() else root / selected)
        for library in short_option_values(event["arguments"], "-l"):
            if not library.startswith("static="):
                continue
            name = library.split("=", 1)[1]
            candidates = []
            for producer in native_nodes:
                if producer["kind"] != "archive":
                    continue
                for output in producer["outputs"]:
                    output_path = root / output["path"]
                    if output_path.name in {f"lib{name}.a", f"{name}.lib"} and output_path.parent in searches:
                        candidates.append((producer["id"], output_path.name))
            if len(candidates) == 1:
                producer, artifact = candidates[0]
                edges.append(
                    {
                        "producer": producer,
                        "consumer": node["id"],
                        "role": "rust_native_archive",
                        "artifact": artifact,
                    }
                )
            elif len(candidates) > 1:
                gaps.append(
                    {
                        "kind": "rust_required_native_operation",
                        "consumer": node["id"],
                        "library": library,
                        "reason": "rust_native_archive_correlation_ambiguous",
                    }
                )
    for node in build_nodes:
        for executable in node["compiled_executables"]:
            matches = build_script_producers[node["action"]["package_hint"]]
            if len(matches) == 1:
                edges.append(
                    {
                        "producer": matches[0],
                        "consumer": node["id"],
                        "role": "build_script_executable",
                        "artifact": executable,
                    }
                )
            else:
                gaps.append(
                    {
                        "kind": "build_script_dependency",
                        "consumer": node["id"],
                        "artifact": executable,
                        "reason": "build_script_compiler_dependency_correlation_ambiguous",
                    }
                )

    nodes = sorted([*compiler_nodes, *build_nodes, *native_nodes], key=lambda node: node["id"])
    edges.sort(key=lambda edge: (edge["producer"], edge["consumer"], edge["role"], edge["artifact"]))
    gaps.sort(key=lambda gap: json.dumps(gap, sort_keys=True, separators=(",", ":")))
    classes = collections.Counter(node["kind"] for node in nodes)
    rust_classes = collections.Counter(
        node["action"]["action_class"] for node in compiler_nodes
    )
    required = {
        "rust_compiler": classes["rust_compiler"] > 0,
        "build_script_execution": classes["build_script_execution"] > 0,
        "proc_macro_producer": rust_classes["proc_macro_producer"] > 0,
        "native_compile": classes["native_compile"] > 0,
        "assembly": classes["assembly"] > 0,
        "preprocessed_assembly": classes["preprocessed_assembly"] > 0,
        "archive": classes["archive"] > 0,
        "native_tool_probe": classes["native_tool_probe"] > 0,
    }
    ambiguous = [
        gap
        for gap in gaps
        if gap["reason"].endswith("_ambiguous")
        or gap["reason"].endswith("_unavailable")
    ]
    return {
        "schema_version": INVENTORY_SCHEMA_VERSION,
        "accounting_complete": not ambiguous and all(required.values()),
        "required_coverage": required,
        "operation_counts": dict(sorted(classes.items())),
        "rust_class_counts": dict(sorted(rust_classes.items())),
        "nodes": nodes,
        "edges": edges,
        "explicit_gaps": gaps,
        "ambiguous_correlations": ambiguous,
    }


def qualify_sccache_rejections(
    missing_hits: list[dict[str, Any]], inventory: dict[str, Any]
) -> bool:
    incoming_proc_macros: collections.defaultdict[str, list[str]] = collections.defaultdict(list)
    for edge in inventory["edges"]:
        if edge["role"] == "proc_macro_dependency":
            incoming_proc_macros[edge["consumer"]].append(edge["producer"])
    nodes_by_action: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    proven_by_action: collections.Counter[str] = collections.Counter()
    for node in inventory["nodes"]:
        if node["kind"] == "rust_compiler":
            action = node["action_id"]
            nodes_by_action[action].append(node)
            if (
                node["outcome"] == "bypass"
                and node["reason"] == "dynamic_dependency_execution_observation_unavailable"
                and incoming_proc_macros[node["id"]]
            ):
                proven_by_action[action] += 1

    complete = True
    for missing in missing_hits:
        observations = []
        for node in nodes_by_action[missing["action_id"]]:
            proc_macros = sorted(incoming_proc_macros[node["id"]])
            observations.append(
                {
                    "node": node["id"],
                    "outcome": node["outcome"],
                    "reason": node["reason"],
                    "proc_macro_producers": proc_macros,
                }
            )
        proven = proven_by_action[missing["action_id"]] > 0
        if proven:
            proven_by_action[missing["action_id"]] -= 1
        missing["cargo_rail_evidence"] = observations
        missing["rejection_proven"] = proven
        complete &= proven
    return complete


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
        action = canonical_action(event["arguments"], event["compiler"])
        if event.get("action") != action:
            raise ValueError(f"coverage event disagrees with compiler operation authority: {path}")
        if event.get("action_id") != action_id(action):
            raise ValueError(f"coverage event has an invalid compiler operation identity: {path}")
        detailed[event["action_id"]].append(event)

    events = []
    for compiler, arguments in cargo_verbose_invocations(verbose_log):
        action = canonical_action(arguments, compiler)
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
                "remote_request_attempts",
                "remote_coordinator_requests",
                "remote_payload_bytes_read",
                "remote_payload_bytes_written",
                "remote_service_elapsed_ns",
                "timing",
                "durability",
                "remote_error",
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


def unrestored_requested_output_roles(event: dict[str, Any]) -> list[str]:
    outputs = event.get("outputs", [])
    # A build script may deliberately delete every output from a successful
    # compiler probe before the Cargo command finishes. Partial retained output
    # is still decisive: the tool restored a result but omitted one requested
    # sibling from the same compiler invocation.
    if not outputs:
        return []
    observed = {output["role"] for output in outputs}
    missing = []
    for requested in event["requested_output_roles"]:
        restored = {
            "dep-info": "dep_info" in observed,
            "metadata": "metadata" in observed,
            "link": bool(observed & {"rlib", "linked_library", "linked_output"}),
        }.get(requested, False)
        if not restored:
            missing.append(requested)
    return missing


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
    unsafe_sccache_hits = []
    sccache_hits = [
        event
        for event in sccache
        if event["language"] == "rust" and event["outcome"] == "hit"
    ]
    safe_sccache_hits = []
    for event in sccache_hits:
        unrestored = unrestored_requested_output_roles(event)
        if unrestored:
            unsafe_sccache_hits.append(
                {
                    "action_id": event["action_id"],
                    "action": event["action"],
                    "reason": "requested_output_not_restored",
                    "requested_output_roles": event["requested_output_roles"],
                    "unrestored_output_roles": unrestored,
                    "observed_outputs": event.get("outputs", []),
                    "rejection_proven": True,
                }
            )
            continue
        safe_sccache_hits.append(event)
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
    sccache_metrics = hit_metrics(safe_sccache_hits)
    complete_costs = cargo_metrics["cold_cost_complete"] and sccache_metrics["cold_cost_complete"]
    strict_superset = (
        bool(extra)
        and cargo_metrics["verified_actions"] > sccache_metrics["verified_actions"]
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
    passed = not ambiguous and complete_costs and strict_superset
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
            "route": (
                "four_dimensional_more_and_faster"
                if passed and more_and_faster
                else "strict_safe_action_superset"
                if passed
                else None
            ),
            "strict_safe_action_superset": strict_superset and complete_costs and not ambiguous,
            "four_dimensional_more_and_faster": more_and_faster and complete_costs and not ambiguous,
            "matched_sccache_hits": len(matched),
            "missing_sccache_hits": missing,
            "unsafe_sccache_hits": unsafe_sccache_hits,
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
    report["operation_inventory"] = operation_inventory(
        cargo_rail,
        arguments.cargo_rail_messages,
        arguments.cargo_rail_verbose,
        arguments.cargo_rail_root,
    )
    rejections_complete = qualify_sccache_rejections(
        report["coverage_gate"]["missing_sccache_hits"], report["operation_inventory"]
    )
    report["coverage_gate"]["sccache_hit_rejections_complete"] = rejections_complete
    report["coverage_gate"]["operation_inventory_accounting_complete"] = report["operation_inventory"][
        "accounting_complete"
    ]
    report["coverage_gate"]["passed"] = (
        report["coverage_gate"]["passed"]
        and report["operation_inventory"]["accounting_complete"]
        and rejections_complete
    )
    report["coverage_gate"]["strict_safe_action_superset"] &= rejections_complete
    if not report["coverage_gate"]["passed"]:
        report["coverage_gate"]["route"] = None
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    with arguments.output.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, sort_keys=True)
        output.write("\n")
    print(json.dumps(report["coverage_gate"], indent=2, sort_keys=True))
    return 0 if report["coverage_gate"]["passed"] else 1


def compiler_mode_report(events: list[dict[str, Any]]) -> dict[str, Any]:
    selected = [
        event
        for event in events
        if event["action"]["driver"] in {"clippy", "rustdoc"}
        and event["action"]["action_class"] != "compiler_request"
    ]

    def matches(event: dict[str, Any], driver: str, test: bool, reason: str) -> bool:
        return (
            event["action"]["driver"] == driver
            and event["action"]["test"] is test
            and event["outcome"] == "bypass"
            and event["reason"] == reason
            and event.get("action_key") is None
            and event.get("result_key") is None
            and event.get("remote_base_action_key") is None
            and event.get("cache_bytes_read", 0) == 0
            and event.get("remote_request_attempts", 0) == 0
            and event.get("remote_coordinator_requests", 0) == 0
            and event.get("remote_payload_bytes_read", 0) == 0
            and event.get("remote_payload_bytes_written", 0) == 0
        )

    required = {
        "clippy_diagnostics": any(
            matches(event, "clippy", False, "clippy_diagnostic_result_authority_unavailable")
            for event in selected
        ),
        "doctest_execution": any(
            matches(event, "rustdoc", True, "doctest_execution_result_authority_unavailable")
            for event in selected
        ),
        "rustdoc_output": any(
            matches(event, "rustdoc", False, "rustdoc_output_tree_observation_unavailable")
            for event in selected
        ),
    }
    violations = []
    for event in selected:
        expected = (
            "clippy_diagnostic_result_authority_unavailable"
            if event["action"]["driver"] == "clippy"
            else "doctest_execution_result_authority_unavailable"
            if event["action"]["test"]
            else "rustdoc_output_tree_observation_unavailable"
        )
        if not matches(event, event["action"]["driver"], event["action"]["test"], expected):
            violations.append(
                {
                    "action_id": event["action_id"],
                    "driver": event["action"]["driver"],
                    "test": event["action"]["test"],
                    "outcome": event["outcome"],
                    "reason": event["reason"],
                }
            )
    return {
        "schema_version": 1,
        "passed": all(required.values()) and not violations,
        "required_coverage": required,
        "selected_operations": len(selected),
        "violations": violations,
        "operations": selected,
    }


def compiler_mode_report_command(arguments: argparse.Namespace) -> int:
    events = load_cargo_rail_events(arguments.cargo_rail_events, arguments.cargo_rail_verbose, arguments.cargo_rail_root)
    report = compiler_mode_report(events)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    with arguments.output.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, sort_keys=True)
        output.write("\n")
    print(json.dumps({key: report[key] for key in ("passed", "required_coverage", "violations")}, indent=2))
    return 0 if report["passed"] else 1


def remote_coverage_report(cargo_rail: list[dict[str, Any]], sccache: list[dict[str, Any]]) -> dict[str, Any]:
    cargo_hits: collections.defaultdict[str, list[dict[str, Any]]] = collections.defaultdict(list)
    cargo_rejections: collections.Counter[str] = collections.Counter()
    for event in cargo_rail:
        if event["language"] != "rust":
            continue
        if event["outcome"] == "hit":
            cargo_hits[event["action_id"]].append(event)
        elif (
            event["outcome"] == "bypass"
            and event["reason"] == "dynamic_dependency_execution_observation_unavailable"
            and "possible_proc_macro_consumer" in event["action"]["capabilities"]
        ):
            cargo_rejections[event["action_id"]] += 1

    matched = []
    missing = []
    unsafe = []
    safe_sccache_hits = 0
    for event in sccache:
        if event["language"] != "rust" or event["outcome"] != "hit":
            continue
        unrestored = unrestored_requested_output_roles(event)
        if unrestored:
            unsafe.append(
                {
                    "action_id": event["action_id"],
                    "reason": "requested_output_not_restored",
                    "unrestored_output_roles": unrestored,
                }
            )
            continue
        if cargo_rejections[event["action_id"]] > 0:
            cargo_rejections[event["action_id"]] -= 1
            unsafe.append(
                {
                    "action_id": event["action_id"],
                    "reason": "unobserved_dynamic_compiler_dependency_execution",
                }
            )
            continue
        safe_sccache_hits += 1
        available = cargo_hits[event["action_id"]]
        if available:
            available.pop()
            matched.append(event["action_id"])
        else:
            missing.append(event["action_id"])

    extra = [identifier for identifier, events in sorted(cargo_hits.items()) for _event in events]
    return {
        "schema_version": 1,
        "passed": safe_sccache_hits > 0 and not missing and len(extra) > 0,
        "cargo_rail_verified_hits": len(matched) + len(extra),
        "safe_sccache_hits": safe_sccache_hits,
        "matched_safe_sccache_hits": len(matched),
        "extra_cargo_rail_hits": extra,
        "missing_safe_sccache_hits": missing,
        "unsafe_sccache_hits": unsafe,
    }


def remote_coverage_report_command(arguments: argparse.Namespace) -> int:
    cargo_rail = load_cargo_rail_events(
        arguments.cargo_rail_events,
        arguments.cargo_rail_verbose,
        arguments.cargo_rail_root,
    )
    sccache = sccache_events(arguments.sccache_log, arguments.sccache_root)
    report = remote_coverage_report(cargo_rail, sccache)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    with arguments.output.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, sort_keys=True)
        output.write("\n")
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


def self_test_command(_arguments: argparse.Namespace) -> int:
    assert cargo_package_label("path+file:///workspace/fixture#0.1.0", "fixture") == "fixture 0.1.0"
    assert (
        cargo_package_label("registry+https://example.invalid/index#fixture-sys@2.3.4", "fixture-sys-2.3.4")
        == "fixture-sys 2.3.4"
    )

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
            "outputs": [
                {
                    "name": f"{identifier}.rmeta",
                    "role": "metadata",
                    "logical_bytes": logical_bytes,
                }
            ],
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
            ("sccache-incomplete", 0.25, 0.25),
        ]
    ]
    cargo = [
        event("shared", "hit", 10, 1.0, 1.0),
        event("cargo-extra", "hit", 50, 3.0, 4.0),
        event("cargo-extra-2", "hit", 5, 1.0, 1.0),
        event("sccache-incomplete", "hit", 1, 0.25, 0.25),
    ]
    sccache = [
        event("shared", "hit", 10, 1.0, 1.0),
        event("sccache-unsafe", "hit", 5, 0.5, 0.5),
        event("sccache-incomplete", "hit", 0, 0.25, 0.25),
    ]
    sccache[-1]["outputs"] = [
        {"name": "sccache-incomplete.d", "role": "dep_info", "logical_bytes": 0}
    ]
    # coverage_report attaches costs from the canonical cost input; remove the
    # synthetic inline copies so this exercises the real completeness path.
    for selected in [*cargo, *sccache]:
        selected.pop("cold_cost")
    report = coverage_report(cargo, sccache, costs)
    assert report["coverage_gate"]["passed"]
    assert report["coverage_gate"]["route"] == "four_dimensional_more_and_faster"
    equal = coverage_report([cargo[0]], [sccache[0]], [costs[0]])
    assert not equal["coverage_gate"]["passed"]
    assert not equal["coverage_gate"]["strict_safe_action_superset"]
    assert report["coverage_gate"]["unsafe_sccache_hits"] == [
        {
            "action_id": "sccache-incomplete",
            "action": {"crate_name": "sccache-incomplete"},
            "reason": "requested_output_not_restored",
            "requested_output_roles": ["metadata"],
            "unrestored_output_roles": ["metadata"],
            "observed_outputs": [
                {"name": "sccache-incomplete.d", "role": "dep_info", "logical_bytes": 0}
            ],
            "rejection_proven": True,
        }
    ]
    incomplete = coverage_report(cargo, sccache, costs[:-2])
    assert not incomplete["coverage_gate"]["passed"]
    assert not incomplete["coverage_gate"]["cost_evidence"]["complete"]
    identity_arguments = [
        "--crate-name",
        "fixture_service",
        "--crate-type=lib",
        "--emit=dep-info,metadata,link",
        "--edition=2024",
        "--cfg",
        'feature="json"',
        "--cfg",
        'fixture_root="/first/generated"',
        "--extern",
        "fixture_macros=/first/target/libfixture_macros.dylib",
        "-L",
        "native=/first/target/native",
        "-lstatic=fixture",
        "-C",
        "linker=/first/tools/fixture-linker",
        "-Clink-arg=-T/first/link/fixture.ld",
        "-Zcodegen-backend=/first/backends/fixture_backend.so",
        "--target=/first/targets/fixture.json",
        "/first/crates/fixture-service/src/lib.rs",
    ]
    assert action_id(canonical_action(identity_arguments)) == (
        "coverage-action-v3:sha256:"
        "1beb6d5ae68b8c50a0a353eefb2db1ecc926ebc32d1c4f81b4b8829a2ee574f1"
    )
    moved_identity_arguments = [argument.replace("/first", "/second") for argument in identity_arguments]
    assert canonical_action(identity_arguments) == canonical_action(moved_identity_arguments)
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
        root = output_dir / "workspace"
        build_output = root / "target/debug/build/fixture-native-sys-output/out"
        build_output.mkdir(parents=True)
        (build_output / "libfixture_native.a").write_bytes(b"archive")
        native_object = build_output / "value.o"
        native_object.write_bytes(b"object")
        native_source = root / "crates/fixture-native-sys/native/value.c"
        native_source.parent.mkdir(parents=True)
        native_source.write_text("int value(void) { return 1; }\n", encoding="utf-8")
        assembly_object = build_output / "plain.o"
        assembly_object.write_bytes(b"assembly object")
        assembly_source = native_source.parent / "plain.s"
        assembly_source.write_text(".text\n", encoding="utf-8")
        preprocessed_object = build_output / "preprocessed.o"
        preprocessed_object.write_bytes(b"preprocessed assembly object")
        preprocessed_source = native_source.parent / "preprocessed.S"
        preprocessed_source.write_text("#define VALUE 1\n.text\n", encoding="utf-8")
        build_executable = root / "target/debug/build/fixture-native-sys-input/build-script-build"
        build_executable.parent.mkdir(parents=True)
        build_executable.write_bytes(b"build script")
        macro_output = root / "target/debug/deps/libfixture_macros.dylib"
        macro_output.parent.mkdir(parents=True)
        macro_output.write_bytes(b"proc macro")
        messages = output_dir / "cargo.stdout"
        artifact = {
            "reason": "compiler-artifact",
            "package_id": "path+file:///workspace/fixture-native-sys#0.1.0",
            "manifest_path": str(root / "crates/fixture-native-sys/Cargo.toml"),
            "target": {
                "kind": ["custom-build"],
                "crate_types": ["bin"],
                "name": "build-script-build",
                "src_path": str(root / "crates/fixture-native-sys/build.rs"),
            },
            "features": [],
            "filenames": [str(build_executable)],
        }
        executed = {
            "reason": "build-script-executed",
            "package_id": artifact["package_id"],
            "linked_libs": ["static=fixture_native"],
            "linked_paths": [str(build_output)],
            "cfgs": [],
            "env": [],
            "out_dir": str(build_output),
        }
        messages.write_text(
            f"{json.dumps(artifact)}\n{json.dumps(executed)}\n", encoding="utf-8"
        )
        verbose = output_dir / "cargo.stderr"
        verbose.write_text(
            "     Running `"
            f"HOST=test-host TARGET=test-target PROFILE=debug OPT_LEVEL=0 "
            f"OUT_DIR={build_output} CARGO_MANIFEST_DIR={root / 'crates/fixture-native-sys'} "
            f"{build_executable}`\n"
            "[fixture-native-sys 0.1.0] running: "
            f'FIXTURE_ROOT="{root}" "cc" "-I{root}/include" "-o" "{native_object}" "-c" "native/value.c"\n'
            "[fixture-native-sys 0.1.0] exit status: 0\n"
            "[fixture-native-sys 0.1.0] running: "
            f'"cc" "-o" "{assembly_object}" "-c" "native/plain.s"\n'
            "[fixture-native-sys 0.1.0] exit status: 0\n"
            "[fixture-native-sys 0.1.0] running: "
            f'"cc" "-o" "{preprocessed_object}" "-c" "native/preprocessed.S"\n'
            "[fixture-native-sys 0.1.0] exit status: 0\n"
            "[fixture-native-sys 0.1.0] running: \"cc\" \"-E\" \"native/missing.c\"\n"
            "[fixture-native-sys 0.1.0] running: "
            f'ZERO_AR_DATE="1" "ar" "cq" "{build_output / "libfixture_native.a"}" '
            f'"{native_object}" "{assembly_object}" "{preprocessed_object}"\n'
            "[fixture-native-sys 0.1.0] exit status: 0\n"
            "[fixture-native-sys 0.1.0] running: "
            f'ZERO_AR_DATE="1" "ar" "s" "{build_output / "libfixture_native.a"}"\n'
            "[fixture-native-sys 0.1.0] exit status: 0\n",
            encoding="utf-8",
        )
        build_action = canonical_action(
            [
                "--crate-name",
                "build_script_build",
                "--crate-type=bin",
                "--emit=dep-info,link",
                str(root / "crates/fixture-native-sys/build.rs"),
            ]
        )
        macro_action = canonical_action(
            [
                "--crate-name",
                "fixture_macros",
                "--crate-type=proc-macro",
                "--emit=dep-info,link",
                str(root / "crates/fixture-macros/src/lib.rs"),
            ]
        )
        consumer_arguments = [
            "--crate-name",
            "fixture_api",
            "--crate-type=lib",
            "--emit=dep-info,metadata",
            "--extern",
            f"fixture_macros={macro_output}",
            str(root / "crates/fixture-api/src/lib.rs"),
        ]
        consumer_action = canonical_action(consumer_arguments)
        compiler_events = [
            {
                "action": build_action,
                "action_id": action_id(build_action),
                "compiler": "rustc",
                "arguments": [
                    "--crate-name",
                    "build_script_build",
                    "--crate-type=bin",
                    "--emit=dep-info,link",
                    str(root / "crates/fixture-native-sys/build.rs"),
                ],
                "current_directory": str(root),
                "outputs": [
                    {
                        "name": build_executable.name,
                        "role": "linked_output",
                        "logical_bytes": build_executable.stat().st_size,
                    }
                ],
                "logical_output_bytes": build_executable.stat().st_size,
                "outcome": "miss",
                "reason": "cache_result_unavailable",
            },
            {
                "action": macro_action,
                "action_id": action_id(macro_action),
                "compiler": "rustc",
                "arguments": [
                    "--crate-name",
                    "fixture_macros",
                    "--crate-type=proc-macro",
                    "--emit=dep-info,link",
                    str(root / "crates/fixture-macros/src/lib.rs"),
                ],
                "current_directory": str(root),
                "outputs": [
                    {
                        "name": macro_output.name,
                        "role": "linked_library",
                        "logical_bytes": macro_output.stat().st_size,
                    }
                ],
                "logical_output_bytes": macro_output.stat().st_size,
                "outcome": "miss",
                "reason": "cache_result_unavailable",
            },
            {
                "action": consumer_action,
                "action_id": action_id(consumer_action),
                "compiler": "rustc",
                "arguments": consumer_arguments,
                "current_directory": str(root),
                "outputs": [],
                "logical_output_bytes": 0,
                "outcome": "bypass",
                "reason": "dynamic_dependency_execution_observation_unavailable",
            },
        ]
        inventory = operation_inventory(compiler_events, messages, verbose, root)
        assert inventory["accounting_complete"]
        assert inventory["required_coverage"] == {
            "rust_compiler": True,
            "build_script_execution": True,
            "proc_macro_producer": True,
            "native_compile": True,
            "assembly": True,
            "preprocessed_assembly": True,
            "archive": True,
            "native_tool_probe": True,
        }
        assert inventory["operation_counts"] == {
            "archive": 2,
            "assembly": 1,
            "build_script_execution": 1,
            "native_compile": 1,
            "native_tool_probe": 1,
            "preprocessed_assembly": 1,
            "rust_compiler": 3,
        }
        assert {edge["role"] for edge in inventory["edges"]} == {
            "build_script_executable",
            "build_script_child_process",
            "native_artifact",
            "native_output_attempt_order",
            "proc_macro_dependency",
        }
        assert not inventory["explicit_gaps"]
        native_nodes = [
            node
            for node in inventory["nodes"]
            if node["kind"]
            in {"archive", "assembly", "native_compile", "native_tool_probe", "preprocessed_assembly"}
        ]
        assert all(node["action"]["schema_version"] == 2 for node in native_nodes)
        assert all(str(root) not in json.dumps(node["action"], sort_keys=True) for node in native_nodes)
        assert {node["reason"] for node in native_nodes} == {
            "native_archive_mutation_transaction_unavailable",
            "assembly_dependency_evidence_unavailable",
            "native_child_terminal_status_unavailable",
            "native_compile_dependency_evidence_unavailable",
            "preprocessed_assembly_dependency_evidence_unavailable",
        }
        archives = [node for node in native_nodes if node["kind"] == "archive"]
        assert sorted(node["output_observation"] for node in archives) == [
            "observed",
            "superseded_by_later_mutation",
        ]
        superseded_archive = next(
            node for node in archives if node["output_observation"] == "superseded_by_later_mutation"
        )
        assert "native_child_output_superseded_by_later_mutation" in superseded_archive["cold_reasons"]
        assert sum(
            edge["role"] == "native_output_attempt_order"
            for edge in inventory["edges"]
        ) == 1
        probe = next(node for node in native_nodes if node["kind"] == "native_tool_probe")
        assert probe["exit_code"] is None
        assert "terminal_status" in probe["missing_capabilities"]
        assert "program_identity" in probe["missing_capabilities"]
        assert "input_bytes" in probe["missing_capabilities"]
        assert probe["unavailable_inputs"] == ["crates/fixture-native-sys/native/missing.c"]
        assert "native_child_program_identity_unavailable" in probe["cold_reasons"]
        assert "native_child_input_bytes_unavailable" in probe["cold_reasons"]
        build_execution = next(
            node for node in inventory["nodes"] if node["kind"] == "build_script_execution"
        )
        assert build_execution["action"]["schema_version"] == 1
        assert build_execution["exit_code"] == 0
        generated_archive = next(
            output
            for output in build_execution["outputs"]
            if output["name"] == "libfixture_native.a"
        )
        assert generated_archive["content_digest"] == (
            f"sha256:{hashlib.sha256(b'archive').hexdigest()}"
        )
        verbose.write_text(
            f"{verbose.read_text(encoding='utf-8')}"
            "[fixture-native-sys 0.1.0] exit status: 0\n",
            encoding="utf-8",
        )
        incomplete = operation_inventory(compiler_events, messages, verbose, root)
        assert not incomplete["accounting_complete"]
        assert incomplete["ambiguous_correlations"][-1]["reason"] == "native_child_launch_unavailable"
        missing = [{"action_id": action_id(consumer_action)}]
        assert qualify_sccache_rejections(missing, inventory)
        assert missing[0]["rejection_proven"]
        repeated = [
            {"action_id": action_id(consumer_action)},
            {"action_id": action_id(consumer_action)},
        ]
        assert not qualify_sccache_rejections(repeated, inventory)
        assert [entry["rejection_proven"] for entry in repeated] == [True, False]
        assert not qualify_sccache_rejections([{"action_id": "unknown"}], inventory)
        mode_events = [
            normalized_event(
                "cargo-rail",
                "clippy-driver",
                ["--crate-name", "fixture", "--crate-type=lib", "--emit=metadata", "src/lib.rs"],
                "bypass",
                "clippy_diagnostic_result_authority_unavailable",
                root,
            ),
            normalized_event(
                "cargo-rail",
                "rustdoc",
                ["--crate-name", "fixture", "--crate-type=lib", "--emit=metadata", "src/lib.rs"],
                "bypass",
                "rustdoc_output_tree_observation_unavailable",
                root,
            ),
            normalized_event(
                "cargo-rail",
                "rustdoc",
                ["--crate-name", "fixture", "--crate-type=lib", "--test", "src/lib.rs"],
                "bypass",
                "doctest_execution_result_authority_unavailable",
                root,
            ),
        ]
        mode_report = compiler_mode_report(mode_events)
        assert mode_report["passed"]
        assert mode_report["required_coverage"] == {
            "clippy_diagnostics": True,
            "doctest_execution": True,
            "rustdoc_output": True,
        }
        shared_arguments = [
            "--crate-name",
            "shared",
            "--crate-type=lib",
            "--emit=dep-info,metadata",
            "src/lib.rs",
        ]
        rejected_arguments = [
            "--crate-name",
            "consumer",
            "--crate-type=lib",
            "--emit=dep-info,metadata",
            "--extern",
            "fixture_macros=target/libfixture_macros.so",
            "src/lib.rs",
        ]
        cargo_remote = [
            normalized_event("cargo-rail", "rustc", shared_arguments, "hit", "verified_remote_result", root),
            normalized_event(
                "cargo-rail",
                "rustc",
                rejected_arguments,
                "bypass",
                "dynamic_dependency_execution_observation_unavailable",
                root,
            ),
            normalized_event(
                "cargo-rail",
                "rustc",
                ["--crate-name", "extra", "--crate-type=lib", "--emit=metadata", "src/lib.rs"],
                "hit",
                "verified_remote_result",
                root,
            ),
        ]
        sccache_remote = [
            normalized_event("sccache", "rustc", shared_arguments, "hit", "cache hit", root),
            normalized_event("sccache", "rustc", rejected_arguments, "hit", "cache hit", root),
        ]
        remote_report = remote_coverage_report(cargo_remote, sccache_remote)
        assert remote_report["passed"]
        assert remote_report["safe_sccache_hits"] == 1
        assert remote_report["matched_safe_sccache_hits"] == 1
        assert [entry["reason"] for entry in remote_report["unsafe_sccache_hits"]] == [
            "unobserved_dynamic_compiler_dependency_execution"
        ]
        assert not remote_coverage_report(cargo_remote, [sccache_remote[1]])["passed"]
    print("native-cache coverage ledger self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    report = subparsers.add_parser("report")
    report.add_argument("--cargo-rail-events", type=Path, required=True)
    report.add_argument("--cargo-rail-messages", type=Path, required=True)
    report.add_argument("--cargo-rail-verbose", type=Path, required=True)
    report.add_argument("--cargo-rail-root", type=Path, required=True)
    report.add_argument("--sccache-log", type=Path, required=True)
    report.add_argument("--sccache-root", type=Path, required=True)
    report.add_argument("--cold-timings", type=Path, required=True)
    report.add_argument("--output", type=Path, required=True)
    report.set_defaults(handler=report_command)
    compiler_modes = subparsers.add_parser("compiler-modes")
    compiler_modes.add_argument("--cargo-rail-events", type=Path, required=True)
    compiler_modes.add_argument("--cargo-rail-verbose", type=Path, required=True)
    compiler_modes.add_argument("--cargo-rail-root", type=Path, required=True)
    compiler_modes.add_argument("--output", type=Path, required=True)
    compiler_modes.set_defaults(handler=compiler_mode_report_command)
    remote = subparsers.add_parser("remote")
    remote.add_argument("--cargo-rail-events", type=Path, required=True)
    remote.add_argument("--cargo-rail-verbose", type=Path, required=True)
    remote.add_argument("--cargo-rail-root", type=Path, required=True)
    remote.add_argument("--sccache-log", type=Path, required=True)
    remote.add_argument("--sccache-root", type=Path, required=True)
    remote.add_argument("--output", type=Path, required=True)
    remote.set_defaults(handler=remote_coverage_report_command)
    self_test = subparsers.add_parser("self-test")
    self_test.set_defaults(handler=self_test_command)
    arguments = parser.parse_args()
    return arguments.handler(arguments)


if __name__ == "__main__":
    sys.exit(main())
