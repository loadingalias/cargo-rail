#!/usr/bin/env python3
"""Create and read the versioned named-work plan without reimplementing policy."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
from typing import Any


class PlanError(RuntimeError):
    """The plan is absent, incompatible, or structurally invalid."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PlanError(message)


ID = re.compile(r"[a-z][a-z0-9.-]*")
EVIDENCE_ID = re.compile(r"evidence:sha256:[0-9a-f]{64}")
PLANNING_EVIDENCE_ID = re.compile(r"planning-evidence-v1:sha256:[0-9a-f]{64}")


def exact_keys(value: dict[str, Any], required: set[str], optional: set[str], subject: str) -> None:
    keys = set(value)
    require(required <= keys, f"{subject} is missing {sorted(required - keys)}")
    require(keys <= required | optional, f"{subject} has unknown fields {sorted(keys - required - optional)}")


def string_list(value: Any, subject: str, *, nonempty: bool = False, unique: bool = True) -> list[str]:
    require(isinstance(value, list) and all(isinstance(item, str) for item in value), f"{subject} must be strings")
    require(not nonempty or bool(value), f"{subject} must not be empty")
    require(not unique or len(value) == len(set(value)), f"{subject} contains duplicates")
    return value


def validate_selection(
    scope: dict[str, Any], work_id: str, evidence: dict[str, Any], references: list[str]
) -> None:
    kind = scope.get("kind")
    if kind == "repository":
        exact_keys(scope, {"kind"}, set(), f"work {work_id} repository scope")
        return
    require(kind in {"cargo", "variants"}, f"work {work_id} has an unknown scope kind")
    exact_keys(scope, {"kind", "selection"}, set(), f"work {work_id} scope")
    selection = scope["selection"]
    require(isinstance(selection, dict), f"work {work_id} selection must be an object")
    selection_kind = selection.get("kind")
    if kind == "cargo":
        required = {"kind", "cargo_args", "targets"}
        optional = {"packages"} if selection_kind == "packages" else set()
        require(selection_kind in {"workspace", "packages"}, f"work {work_id} has an unknown Cargo selection")
        if selection_kind == "packages":
            required.add("packages")
            optional.clear()
        exact_keys(selection, required, optional, f"work {work_id} Cargo selection")
        cargo_argv = string_list(selection["cargo_args"], f"work {work_id} Cargo argv", unique=False)
        targets = selection["targets"]
        require(isinstance(targets, list), f"work {work_id} targets must be an array")
        for target in targets:
            require(isinstance(target, dict), f"work {work_id} target must be an object")
            exact_keys(target, {"package", "name", "kind"}, set(), f"work {work_id} target")
            require(isinstance(target["package"], str) and isinstance(target["name"], str), f"work {work_id} target is malformed")
            string_list(target["kind"], f"work {work_id} target kinds")
        if selection_kind == "packages":
            packages = selection["packages"]
            require(isinstance(packages, list) and packages, f"work {work_id} packages must not be empty")
            package_keys = []
            cargo_specs = []
            for package in packages:
                require(isinstance(package, dict), f"work {work_id} package must be an object")
                exact_keys(package, {"key", "name", "cargo_spec"}, set(), f"work {work_id} package")
                require(all(isinstance(package[field], str) and package[field] for field in package), f"work {work_id} package is malformed")
                require(not package["cargo_spec"].startswith("-"), f"work {work_id} Cargo package spec is option-like")
                package_keys.append(package["key"])
                cargo_specs.append(package["cargo_spec"])
            require(package_keys == sorted(set(package_keys)), f"work {work_id} packages are not canonical")
            require(
                cargo_argv == [argument for spec in cargo_specs for argument in ("-p", spec)],
                f"work {work_id} Cargo argv disagrees with typed packages",
            )
            require(
                all(target["package"] in package_keys for target in targets),
                f"work {work_id} target references an unselected package",
            )
        else:
            require(not cargo_argv and not targets, f"work {work_id} workspace selection must not carry narrowing selectors")
        return

    require(selection_kind in {"all", "selected"}, f"work {work_id} has an unknown variant selection")
    if selection_kind == "all":
        exact_keys(selection, {"kind", "evidence"}, set(), f"work {work_id} all-variant selection")
    else:
        exact_keys(selection, {"kind", "variants", "evidence"}, set(), f"work {work_id} variant selection")
        variants = selection["variants"]
        require(isinstance(variants, list) and variants, f"work {work_id} selected variants must not be empty")
        for variant in variants:
            require(isinstance(variant, dict), f"work {work_id} variant must be an object")
            exact_keys(variant, {"id", "dimensions"}, set(), f"work {work_id} variant")
            require(isinstance(variant["id"], str) and ID.fullmatch(variant["id"]), f"work {work_id} variant ID is malformed")
            dimensions = variant["dimensions"]
            require(
                isinstance(dimensions, dict)
                and all(isinstance(name, str) and isinstance(value, (str, int, bool)) for name, value in dimensions.items()),
                f"work {work_id} variant dimensions are malformed",
            )
    reference = selection["evidence"]
    require(isinstance(reference, str) and reference in evidence, f"work {work_id} variant evidence is unknown")
    require(reference in references, f"work {work_id} variant evidence is absent from its decision")
    require(evidence[reference]["subject"] == work_id, f"work {work_id} variant evidence belongs to another work item")


def validate_changes(changes: Any) -> None:
    require(isinstance(changes, dict), "plan changes must be an object")
    exact_keys(changes, {"files", "cargo", "config"}, set(), "plan changes")
    for field in changes:
        require(isinstance(changes[field], list), f"plan changes.{field} must be an array")
    for change in changes["files"]:
        require(isinstance(change, dict), "file change must be an object")
        exact_keys(change, {"path", "kind", "provenance"}, {"relation", "before", "after"}, "file change")
        require(isinstance(change["path"], str) and change["path"], "file change path is malformed")
        require(change["kind"] in {"added", "modified", "type_changed", "deleted"}, "file change kind is malformed")
        provenance = string_list(change["provenance"], "file change provenance")
        require(set(provenance) <= {"committed", "staged", "unstaged", "untracked"}, "file change provenance is unknown")
    for change in changes["cargo"]:
        require(isinstance(change, dict), "Cargo change must be an object")
        exact_keys(change, {"package", "target", "kind"}, set(), "Cargo change")
        require(isinstance(change["package"], str) and isinstance(change["kind"], str), "Cargo change is malformed")
        require(change["target"] is None or isinstance(change["target"], str), "Cargo change target is malformed")
    for change in changes["config"]:
        require(isinstance(change, dict), "config change must be an object")
        exact_keys(change, {"path", "before", "after"}, set(), "config change")
        require(isinstance(change["path"], str) and change["path"], "config change path is malformed")


def load_plan(path: pathlib.Path) -> dict[str, Any]:
    try:
        require(path.stat().st_size <= 64 * 1024 * 1024, "plan exceeds the 64 MiB consumer bound")
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PlanError(f"cannot load plan {path}: {error}") from error
    require(isinstance(value, dict), "plan must be an object")
    exact_keys(value, {"plan_contract_version", "identity", "inputs", "changes", "work", "required", "evidence"}, set(), "plan")
    require(value.get("plan_contract_version") == 8, "plan contract version must be 8")
    identity = value.get("identity")
    require(
        isinstance(identity, str)
        and re.fullmatch(r"plan-v8:sha256:[0-9a-f]{64}", identity) is not None,
        "plan identity is missing or malformed",
    )
    inputs = value["inputs"]
    require(isinstance(inputs, dict), "plan inputs must be an object")
    exact_keys(
        inputs,
        {"base", "head", "head_commit", "capture", "cargo", "configuration", "toolchain", "target", "platform", "catalog", "evidence", "override"},
        set(),
        "plan inputs",
    )
    patterns = {
        "head_commit": r"[0-9a-f]{40,64}",
        "cargo": r"resolution-universe-v[0-9]+:sha256:[0-9a-f]{64}",
        "configuration": r"cargo-configuration-v1:sha256:[0-9a-f]{64}",
        "target": r"planning-target-v1:sha256:[0-9a-f]{64}",
        "catalog": r"work-catalog-v1:sha256:[0-9a-f]{64}",
    }
    for field in ["base", "head", "toolchain", "platform"]:
        require(isinstance(inputs[field], str), f"plan input {field} must be a string")
    for field, pattern in patterns.items():
        require(isinstance(inputs[field], str) and re.fullmatch(pattern, inputs[field]), f"plan input {field} is malformed")
    require(inputs["capture"] is None or isinstance(inputs["capture"], str), "plan capture is malformed")
    input_evidence = string_list(inputs["evidence"], "plan evidence identities")
    require(
        all(PLANNING_EVIDENCE_ID.fullmatch(reference) for reference in input_evidence),
        "plan evidence identities are malformed",
    )
    require(inputs["override"] in {"none", "all"}, "plan override is malformed")
    validate_changes(value["changes"])

    work = value["work"]
    required = value["required"]
    evidence = value["evidence"]
    require(isinstance(work, dict), "plan work must be an object")
    required = string_list(required, "plan required work")
    require(required == sorted(required), "plan required work must be sorted")
    require(isinstance(evidence, dict), "plan evidence must be an object")
    for reference, record in evidence.items():
        require(isinstance(reference, str) and EVIDENCE_ID.fullmatch(reference), "plan evidence ID is malformed")
        require(isinstance(record, dict), f"plan evidence {reference} must be an object")
        exact_keys(record, {"code", "subject", "description", "input", "complete"}, set(), f"plan evidence {reference}")
        require(all(isinstance(record[field], str) and record[field] for field in ["code", "subject", "description"]), f"plan evidence {reference} is malformed")
        require(record["input"] is None or isinstance(record["input"], str), f"plan evidence {reference} input is malformed")
        require(isinstance(record["complete"], bool), f"plan evidence {reference} completeness is malformed")
        portable_record = {
            "code": record["code"],
            "subject": record["subject"],
            "input": record["input"],
            "complete": record["complete"],
        }
        encoded_record = json.dumps(
            portable_record, ensure_ascii=False, separators=(",", ":"), sort_keys=True
        ).encode("utf-8")
        require(
            reference == f"evidence:sha256:{hashlib.sha256(encoded_record).hexdigest()}",
            f"plan evidence {reference} does not match its typed content",
        )
    projected: list[str] = []
    for work_id, decision in sorted(work.items()):
        require(isinstance(work_id, str) and ID.fullmatch(work_id) and isinstance(decision, dict), "plan work decisions are malformed")
        state = decision.get("state")
        require(state in {"required", "skipped"}, f"work {work_id} has an invalid state")
        references = string_list(decision.get("evidence"), f"work {work_id} evidence", nonempty=True)
        require(all(reference in evidence for reference in references), f"work {work_id} references unknown evidence")
        require(all(evidence[reference]["subject"] == work_id for reference in references), f"work {work_id} references another work item's evidence")
        if state == "required":
            exact_keys(decision, {"state", "cause", "scope", "evidence"}, set(), f"required work {work_id}")
            require(decision["cause"] in {"changed_input", "incomplete_evidence", "forced_all"}, f"work {work_id} cause is malformed")
            require(isinstance(decision["scope"], dict), f"required work {work_id} has no scope")
            validate_selection(decision["scope"], work_id, evidence, references)
            if decision["cause"] == "incomplete_evidence":
                require(
                    any(not evidence[reference]["complete"] for reference in references),
                    f"work {work_id} incomplete decision has no incomplete evidence",
                )
            projected.append(work_id)
        else:
            exact_keys(decision, {"state", "evidence"}, set(), f"skipped work {work_id}")
            require(
                all(evidence[reference]["complete"] for reference in references),
                f"skipped work {work_id} cites incomplete evidence",
            )
    require(required == projected, "required work projection disagrees with work decisions")
    portable = {
        "plan_contract_version": value["plan_contract_version"],
        "inputs": inputs,
        "changes": value["changes"],
        "work": work,
        "required": required,
        "evidence": sorted(evidence),
    }
    encoded = json.dumps(portable, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")
    expected_identity = f"plan-v8:sha256:{hashlib.sha256(encoded).hexdigest()}"
    require(identity == expected_identity, "plan identity does not match its typed portable content")
    return value


def decision(plan: dict[str, Any], work_id: str) -> dict[str, Any]:
    value = plan["work"].get(work_id)
    require(isinstance(value, dict), f"plan does not register work {work_id}")
    return value


def create_plan(path: pathlib.Path, force_all: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    binary = os.environ.get("CARGO_RAIL_BIN")
    if binary:
        command = [binary, "rail", "plan"]
    else:
        bootstrap_target = os.environ.get("RAIL_BOOTSTRAP_TARGET_DIR", "target/cargo-rail-bootstrap")
        command = [
            "cargo",
            "run",
            "--quiet",
            "--locked",
            "--target-dir",
            bootstrap_target,
            "--",
            "rail",
            "plan",
        ]
    since = os.environ.get("RAIL_SINCE")
    zero_before = bool(since) and set(since) == {"0"}
    if zero_before:
        since = None
    if since:
        command.extend(["--since", since])
    command.append("--json")
    evidence = os.environ.get("CARGO_RAIL_PLANNING_EVIDENCE")
    if evidence:
        command.extend(["--evidence", evidence])
    if force_all or zero_before:
        command.append("--all")
    with path.open("wb") as output:
        result = subprocess.run(command, check=False, stdout=output)
    if result.returncode != 0:
        raise PlanError(f"cargo-rail planning failed with exit code {result.returncode}")
    load_plan(path)


def cargo_args(plan: dict[str, Any], work_id: str) -> list[str]:
    selected = decision(plan, work_id)
    if selected["state"] == "skipped":
        return []
    scope = selected["scope"]
    require(scope.get("kind") == "cargo", f"work {work_id} does not have Cargo scope")
    selection = scope.get("selection")
    require(isinstance(selection, dict), f"work {work_id} has no Cargo selection")
    arguments = selection.get("cargo_args")
    require(isinstance(arguments, list) and all(isinstance(item, str) for item in arguments), f"work {work_id} Cargo argv is malformed")
    return arguments


def target_args(plan: dict[str, Any], work_id: str) -> list[str]:
    selected = decision(plan, work_id)
    if selected["state"] == "skipped":
        return []
    selection = selected["scope"].get("selection", {})
    targets = selection.get("targets")
    require(isinstance(targets, list), f"work {work_id} targets are malformed")
    names: list[str] = []
    for target in targets:
        require(isinstance(target, dict), f"work {work_id} target is malformed")
        kinds = target.get("kind")
        name = target.get("name")
        require(isinstance(kinds, list) and isinstance(name, str), f"work {work_id} target is malformed")
        if "test" in kinds:
            names.append(name)
    return [argument for name in sorted(set(names)) for argument in ("--test", name)]


def matrix(plan: dict[str, Any], work_id: str, family: str | None) -> Any:
    selected = decision(plan, work_id)
    if selected["state"] == "skipped":
        return {"include": []}
    scope = selected["scope"]
    require(scope.get("kind") == "variants", f"work {work_id} does not have variant scope")
    selection = scope.get("selection")
    require(isinstance(selection, dict), f"work {work_id} variant selection is malformed")
    if selection.get("kind") == "all":
        return "all"
    require(selection.get("kind") == "selected", f"work {work_id} variant selection is malformed")
    variants = selection.get("variants")
    require(isinstance(variants, list) and variants, f"work {work_id} selected no variants")
    rows = []
    for variant in variants:
        require(isinstance(variant, dict) and isinstance(variant.get("dimensions"), dict), "variant row is malformed")
        dimensions = dict(variant["dimensions"])
        row_family = dimensions.pop("family", None)
        if family is not None and row_family != family:
            continue
        row = {"id": variant["id"], **dimensions}
        rows.append({family: row} if family is not None else row)
    if family is None:
        require(rows, f"work {work_id} selected no variants")
    return {"include": rows}


def verify_checkout(path: pathlib.Path) -> None:
    configured = os.environ.get("CARGO_RAIL_BIN")
    if configured:
        command = [configured, "rail"]
    else:
        script = pathlib.Path(__file__).resolve()
        repository = script.parents[2] if len(script.parents) > 2 else None
        if repository is not None and (repository / "Cargo.toml").is_file():
            bootstrap_target = os.environ.get("RAIL_BOOTSTRAP_TARGET_DIR", "target/cargo-rail-bootstrap")
            command = [
                "cargo",
                "run",
                "--quiet",
                "--locked",
                "--target-dir",
                bootstrap_target,
                "--",
                "rail",
            ]
        else:
            binary = shutil.which("cargo-rail")
            require(binary is not None, "matching cargo-rail binary is unavailable for saved-plan verification")
            command = [binary, "rail"]
    result = subprocess.run(
        [*command, "plan", "--verify", str(path.resolve())],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    require(result.returncode == 0, f"cargo-rail rejected current execution authority: {result.stderr.decode(errors='replace').strip()}")
    require(not result.stdout, "cargo-rail saved-plan verification emitted unexpected stdout")


def emit_null(arguments: list[str]) -> None:
    sys.stdout.buffer.write(b"".join(argument.encode("utf-8") + b"\0" for argument in arguments))


def emit_line(value: str) -> None:
    """Write one machine line without platform newline translation."""
    sys.stdout.buffer.write(value.encode("utf-8") + b"\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("path", type=pathlib.Path)
    create.add_argument("--all", action="store_true")
    validate = commands.add_parser("validate")
    validate.add_argument("path", type=pathlib.Path)
    required = commands.add_parser("required")
    required.add_argument("path", type=pathlib.Path)
    identity = commands.add_parser("identity")
    identity.add_argument("path", type=pathlib.Path)
    is_required = commands.add_parser("is-required")
    is_required.add_argument("path", type=pathlib.Path)
    is_required.add_argument("work")
    cargo = commands.add_parser("cargo-args")
    cargo.add_argument("path", type=pathlib.Path)
    cargo.add_argument("work")
    targets = commands.add_parser("target-args")
    targets.add_argument("path", type=pathlib.Path)
    targets.add_argument("work")
    variants = commands.add_parser("matrix")
    variants.add_argument("path", type=pathlib.Path)
    variants.add_argument("work")
    variants.add_argument("--family")
    checkout = commands.add_parser("verify-checkout")
    checkout.add_argument("path", type=pathlib.Path)
    arguments = parser.parse_args()

    if arguments.command == "create":
        create_plan(arguments.path, arguments.all)
        return 0
    plan = load_plan(arguments.path)
    if arguments.command == "validate":
        return 0
    if arguments.command == "required":
        emit_line(json.dumps(plan["required"], separators=(",", ":")))
    elif arguments.command == "identity":
        emit_line(plan["identity"])
    elif arguments.command == "is-required":
        emit_line("true" if decision(plan, arguments.work)["state"] == "required" else "false")
    elif arguments.command == "cargo-args":
        emit_null(cargo_args(plan, arguments.work))
    elif arguments.command == "target-args":
        emit_null(target_args(plan, arguments.work))
    elif arguments.command == "matrix":
        selected = matrix(plan, arguments.work, arguments.family)
        emit_line("all" if selected == "all" else json.dumps(selected, separators=(",", ":")))
    elif arguments.command == "verify-checkout":
        verify_checkout(arguments.path)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PlanError as error:
        print(f"plan reader: {error}", file=sys.stderr)
        raise SystemExit(2) from error
