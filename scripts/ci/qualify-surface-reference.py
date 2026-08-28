#!/usr/bin/env python3
"""Qualify one native Cargo-Rail artifact against a pinned reference analyzer."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path
from typing import Any

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MANIFEST = REPOSITORY_ROOT / "tests/surface-reference.json"
LANES = ("cold-target", "cargo-fresh", "fact-cache-hit", "cache-bypass")


class QualificationError(RuntimeError):
    """One pinned input, analyzer run, or semantic comparison is invalid."""


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("--reference-archive", required=True, type=Path)
    value.add_argument("--cargo-rail", required=True, type=Path)
    value.add_argument("--output", required=True, type=Path)
    value.add_argument("--runs", type=int, default=20)
    value.add_argument("--conformance-only", action="store_true")
    return value


def require(condition: bool, message: str) -> None:
    if not condition:
        raise QualificationError(message)


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise QualificationError(f"cannot load {path}: {error}") from error


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def bytes_sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def tree_sha256(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(path for path in root.rglob("*") if path.is_file()):
        relative = path.relative_to(root).as_posix().encode()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        contents = path.read_bytes()
        digest.update(len(contents).to_bytes(8, "big"))
        digest.update(contents)
    return digest.hexdigest()


def command_output(command: list[str], environment: dict[str, str] | None = None) -> str:
    completed = subprocess.run(command, env=environment, check=True, capture_output=True)
    return completed.stdout.decode().strip()


def extract_archive(archive: Path, destination: Path) -> Path:
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        for member in members:
            resolved = (destination / member.name).resolve()
            require(
                resolved == destination.resolve() or destination.resolve() in resolved.parents,
                f"reference archive member escapes extraction root: {member.name}",
            )
            require(not member.issym() and not member.islnk(), f"reference archive contains link: {member.name}")
        source.extractall(destination, members=members, filter="data")
    manifests = list(destination.glob("*/Cargo.toml"))
    require(len(manifests) == 1, "reference archive must contain one top-level Cargo.toml")
    return manifests[0].parent


def build_reference(source: Path, rust: str, output: Path) -> Path:
    environment = os.environ.copy()
    environment.update({"RUSTUP_TOOLCHAIN": rust, "CARGO_INCREMENTAL": "0"})
    completed = subprocess.run(
        ["cargo", "build", "--release", "--locked"],
        cwd=source,
        env=environment,
        check=False,
        capture_output=True,
    )
    (output / "reference-build.stdout").write_bytes(completed.stdout)
    (output / "reference-build.stderr").write_bytes(completed.stderr)
    require(completed.returncode == 0, "pinned reference source failed to build")
    binary = source / "target/release/cargo-hawk"
    require(binary.is_file(), "pinned reference build did not produce its declared binary")
    return binary


def time_command(command: list[str], measurement: Path) -> list[str]:
    timer = Path("/usr/bin/time")
    require(timer.is_file(), "surface qualification requires /usr/bin/time")
    if sys.platform == "darwin":
        return [str(timer), "-l", "-o", str(measurement), *command]
    if sys.platform.startswith("linux"):
        return [str(timer), "-v", "-o", str(measurement), *command]
    raise QualificationError("surface performance qualification supports Linux and macOS hosts")


def parse_time(path: Path) -> dict[str, float | int]:
    source = path.read_text(encoding="utf-8")
    if sys.platform == "darwin":
        cpu = re.search(r"([0-9.]+) real\s+([0-9.]+) user\s+([0-9.]+) sys", source)
        rss = re.search(r"([0-9]+)\s+maximum resident set size", source)
        require(cpu is not None and rss is not None, "cannot parse macOS time evidence")
        return {
            "timer_elapsed_seconds": float(cpu.group(1)),
            "user_cpu_seconds": float(cpu.group(2)),
            "system_cpu_seconds": float(cpu.group(3)),
            "peak_rss_bytes": int(rss.group(1)),
        }
    user = re.search(r"User time \(seconds\):\s+([0-9.]+)", source)
    system = re.search(r"System time \(seconds\):\s+([0-9.]+)", source)
    rss = re.search(r"Maximum resident set size \(kbytes\):\s+([0-9]+)", source)
    require(user is not None and system is not None and rss is not None, "cannot parse Linux time evidence")
    return {
        "user_cpu_seconds": float(user.group(1)),
        "system_cpu_seconds": float(system.group(1)),
        "peak_rss_bytes": int(rss.group(1)) * 1024,
    }


def graph_state(root: Path | None) -> dict[Path, tuple[int, int]]:
    if root is None or not root.exists():
        return {}
    return {path: (path.stat().st_mtime_ns, path.stat().st_size) for path in root.rglob("*.json")}


def run_analyzer(
    name: str,
    case_name: str,
    lane: str,
    command: list[str],
    workspace: Path,
    environment: dict[str, str],
    evidence: Path,
    graph_dir: Path | None,
    manifest: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    previous_graph = graph_state(graph_dir)
    time_evidence = evidence.with_suffix(".time.txt")
    started = time.perf_counter_ns()
    process = subprocess.run(
        time_command(command, time_evidence),
        cwd=workspace,
        env=environment,
        check=False,
        capture_output=True,
    )
    elapsed = (time.perf_counter_ns() - started) / 1_000_000_000
    evidence.with_suffix(".stdout").write_bytes(process.stdout)
    evidence.with_suffix(".stderr").write_bytes(process.stderr)
    require(process.returncode in (0, 1), f"{name} acquisition failed with exit {process.returncode}")
    try:
        report = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise QualificationError(f"{name} did not emit one JSON value: {error}") from error
    measurement: dict[str, Any] = {
        "analyzer": name,
        "case": case_name,
        "lane": lane,
        "argv": command,
        "elapsed_seconds": elapsed,
        "exit_code": process.returncode,
        **parse_time(time_evidence),
    }
    if name == "cargo-rail":
        acquisition = report.get("metrics", {}).get("acquisition", {})
        measurement.update(
            {
                "cargo_views": acquisition.get("cargo_views_executed"),
                "compiler_invocations": acquisition.get("compiler_invocations"),
                "fact_cache": {
                    "hits": acquisition.get("fact_cache_hits"),
                    "misses": acquisition.get("fact_cache_misses"),
                    "store_failures": acquisition.get("fact_cache_store_failures"),
                    "bypass_reasons": acquisition.get("fact_cache_bypass_reasons"),
                },
            }
        )
    else:
        current_graph = graph_state(graph_dir)
        changed_fragments = sum(previous_graph.get(path) != state for path, state in current_graph.items())
        profiles = len(manifest["feature_profiles"])
        doctest_views = int(bool(manifest["doctest_packages"]))
        production_views = len(report.get("summary", {}).get("production", []))
        measurement.update(
            {
                "cargo_views": profiles * (production_views + 1 + doctest_views),
                "compiler_invocations": changed_fragments,
                "fact_cache": "not-applicable",
            }
        )
    evidence.with_suffix(".measurement.json").write_text(json.dumps(measurement, indent=2) + "\n", encoding="utf-8")
    return report, measurement


def normalized_reference(report: dict[str, Any]) -> list[dict[str, Any]]:
    require(report.get("schema_version") == 5, "reference report schema changed")
    findings = []
    replacements = {
        "dead_public": None,
        "unnecessary_public": "pub(crate)",
        "unnecessary_restricted_visibility": "",
        "unnecessary_crate_visibility": "pub(super)",
    }
    kinds = {
        "inherent_method": "method",
        "inherent_associated_constant": "associated-constant",
        "type_alias": "type-alias",
        "enum_variant": "variant",
    }
    for diagnostic in report.get("diagnostics", []):
        if diagnostic.get("category") != "finding":
            continue
        identity = diagnostic["identity"]
        location = diagnostic.get("location") or {}
        kind = diagnostic["kind"].replace("_", "-")
        findings.append(
            {
                "package": identity.get("package"),
                "crate": identity["crate"],
                "item": identity["item"],
                "item_kind": kinds.get(identity["kind"], identity["kind"].replace("_", "-")),
                "source": location.get("file"),
                "declaration_start": location.get("byte_start"),
                "finding": kind,
                "replacement": replacements[diagnostic["kind"]],
            }
        )
    return sorted(findings, key=semantic_key)


def normalized_rail(report: dict[str, Any]) -> list[dict[str, Any]]:
    require(report.get("surface_contract_version") == 2, "Cargo-Rail surface contract changed")
    require(report.get("completeness", {}).get("complete") is True, "Cargo-Rail evidence is incomplete")
    require(report.get("authority", {}).get("audited_targets"), "Cargo-Rail audited no closed targets")
    findings = []
    for finding in report.get("findings", []):
        compiler_crates = finding["compiler_crates"]
        findings.append(
            {
                "package": finding["packages"][0] if finding["packages"] else None,
                "crate": compiler_crates[0]["crate_name"] if compiler_crates else None,
                "item": finding["diagnostic_paths"][0],
                "item_kind": finding["item_kind"],
                "source": finding["source"],
                "declaration_start": finding["declaration_start"],
                "finding": finding["kind"],
                "replacement": "" if finding["replacement"] == "private" else finding["replacement"],
            }
        )
    return sorted(findings, key=semantic_key)


def semantic_key(finding: dict[str, Any]) -> tuple[str, ...]:
    return tuple("" if finding.get(field) is None else str(finding[field]) for field in sorted(finding))


def apply_allowlist(
    reference: list[dict[str, Any]], rail: list[dict[str, Any]], allowlist: dict[str, Any]
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    require(set(allowlist) == {"schema_version", "differences"}, "surface allowlist fields changed")
    require(allowlist["schema_version"] == 1, "surface allowlist version changed")
    left = list(reference)
    right = list(rail)
    seen: set[tuple[str, tuple[str, ...]]] = set()
    for index, entry in enumerate(allowlist["differences"]):
        require(
            set(entry) == {"tool", "finding", "reason", "owner"},
            f"allowlist difference[{index}] must contain exact tool, finding, reason, and owner fields",
        )
        require(entry["tool"] in ("reference", "cargo-rail"), f"allowlist difference[{index}] has unknown tool")
        require(isinstance(entry["reason"], str) and entry["reason"], f"allowlist difference[{index}] lacks reason")
        require(isinstance(entry["owner"], str) and entry["owner"], f"allowlist difference[{index}] lacks owner")
        key = (entry["tool"], semantic_key(entry["finding"]))
        require(key not in seen, f"allowlist difference[{index}] duplicates an earlier exception")
        seen.add(key)
        selected = left if entry["tool"] == "reference" else right
        require(entry["finding"] in selected, f"allowlist difference[{index}] is stale or inexact")
        selected.remove(entry["finding"])
    return left, right


def percentile(values: list[float], percentile_value: float) -> float:
    ordered = sorted(values)
    rank = max(0, min(len(ordered) - 1, int((len(ordered) - 1) * percentile_value)))
    return ordered[rank]


def prepare_workspace(source: Path, destination: Path, bypass: bool, rail_config: str) -> None:
    shutil.copytree(source, destination)
    if rail_config != ".config/rail.toml":
        shutil.copyfile(source / rail_config, destination / ".config/rail.toml")
    if bypass:
        manifest = destination / "core/Cargo.toml"
        contents = manifest.read_text(encoding="utf-8")
        manifest.write_text(contents.replace("[package]\n", '[package]\nbuild = "build.rs"\n', 1), encoding="utf-8")
        (destination / "core/build.rs").write_text("fn main() {}\n", encoding="utf-8")


def analyzer_command(name: str, binary: Path, target_dir: Path, graph_dir: Path, reference_config: str) -> list[str]:
    if name == "reference":
        return [
            str(binary),
            "hawk",
            "check",
            "--manifest-path",
            "Cargo.toml",
            "--config",
            reference_config,
            "--target-dir",
            str(target_dir),
            "--graph-dir",
            str(graph_dir),
            "--output-format",
            "json",
            "-D",
            "warnings",
            "-D",
            "hawk::unnecessary_crate_visibility",
        ]
    return [str(binary), "rail", "surface", "--check", "--format", "json"]


def execute_pair(
    case: dict[str, str],
    lane: str,
    index: int,
    workspace_root: Path,
    state_root: Path,
    evidence_root: Path,
    fixture: Path,
    binaries: dict[str, Path],
    environment: dict[str, str],
    manifest: dict[str, Any],
    persistent: bool,
) -> list[dict[str, Any]]:
    bypass = lane in ("cargo-fresh", "cache-bypass")
    measurements = []
    normalized: dict[str, list[dict[str, Any]]] = {}
    order = ("reference", "cargo-rail") if index % 2 == 0 else ("cargo-rail", "reference")
    for analyzer in order:
        workspace = workspace_root / analyzer if persistent else workspace_root / f"{index:02d}-{analyzer}"
        if not workspace.exists():
            prepare_workspace(fixture, workspace, bypass, case["rail_config"])
        target_dir = state_root / analyzer / "target" if persistent else state_root / f"{index:02d}-{analyzer}-target"
        graph_dir = state_root / analyzer / "graph" if persistent else state_root / f"{index:02d}-{analyzer}-graph"
        cache_dir = state_root / "rail-cache" if persistent else state_root / f"{index:02d}-rail-cache"
        graph_dir.mkdir(parents=True, exist_ok=True)
        run_environment = environment.copy()
        run_environment["CARGO_RAIL_CACHE_DIR"] = str(cache_dir)
        evidence = evidence_root / f"{case['name']}-{lane}-{index:02d}-{analyzer}"
        report, measurement = run_analyzer(
            analyzer,
            case["name"],
            lane,
            analyzer_command(analyzer, binaries[analyzer], target_dir, graph_dir, case["reference_config"]),
            workspace,
            run_environment,
            evidence,
            graph_dir if analyzer == "reference" else None,
            manifest,
        )
        normalized[analyzer] = normalized_reference(report) if analyzer == "reference" else normalized_rail(report)
        evidence.with_suffix(".normalized.json").write_text(
            json.dumps(normalized[analyzer], indent=2) + "\n", encoding="utf-8"
        )
        measurements.append(measurement)
    allowlist = load_json(REPOSITORY_ROOT / case["allowlist"])
    unexplained_reference, unexplained_rail = apply_allowlist(
        normalized["reference"], normalized["cargo-rail"], allowlist
    )
    require(unexplained_reference == unexplained_rail, f"unexplained normalized finding mismatch in {lane} run {index}")
    return measurements


def validate_lane(measurements: list[dict[str, Any]]) -> None:
    for measurement in measurements:
        if measurement["analyzer"] != "cargo-rail":
            continue
        cache = measurement["fact_cache"]
        lane = measurement["lane"]
        if lane == "fact-cache-hit":
            require(measurement["cargo_views"] == 0, "fact-cache-hit sample unexpectedly invoked Cargo")
            require(measurement["compiler_invocations"] == 0, "fact-cache-hit sample unexpectedly invoked rustc")
            require(cache["hits"] > 0, "fact-cache-hit sample did not hit complete compiler facts")
            require(cache["misses"] == 0, "fact-cache-hit sample missed complete compiler facts")
            require(cache["store_failures"] == 0, "fact-cache-hit sample failed to store compiler facts")
            require(not cache["bypass_reasons"], "fact-cache-hit sample conservatively bypassed reusable facts")
        if lane in ("cargo-fresh", "cache-bypass"):
            require(cache["bypass_reasons"], f"{lane} sample did not record a conservative cache bypass")


def main() -> int:
    arguments = parser().parse_args()
    arguments.output = arguments.output.resolve()
    require(arguments.runs >= 20 or arguments.conformance_only, "performance qualification requires at least 20 runs")
    manifest = load_json(MANIFEST)
    require(manifest["schema_version"] == 1, "surface conformance manifest version changed")
    reference_authority = manifest["reference"]
    require(reference_authority["version"] == "0.1.13", "unexpected reference version")
    require(
        reference_authority["commit"] == "a3b75f193b931d11cf8883c44bda3f9a79c8f19a",
        "unexpected reference commit",
    )
    require(
        sha256(arguments.reference_archive) == reference_authority["archive_sha256"],
        "reference archive digest mismatch",
    )
    rail_binary = arguments.cargo_rail.resolve()
    require(rail_binary.is_file(), "installed cargo-rail artifact is missing")
    rail_driver = rail_binary.with_name("cargo-rail-fact-driver")
    require(rail_driver.is_file(), "native cargo-rail artifact has no adjacent compiler-fact driver")
    arguments.output.mkdir(parents=True, exist_ok=False)

    fixture = REPOSITORY_ROOT / manifest["fixture"]
    cases = manifest["cases"]
    require(isinstance(cases, list) and cases, "surface conformance cases are empty")
    case_names = [case["name"] for case in cases]
    require(len(case_names) == len(set(case_names)), "surface conformance case names are not unique")
    performance_case = next(
        (case for case in cases if case["name"] == manifest["performance_case"]),
        None,
    )
    require(performance_case is not None, "surface performance case is missing")
    environment = os.environ.copy()
    environment.update({"RUSTUP_TOOLCHAIN": reference_authority["rust"], "CARGO_INCREMENTAL": "0"})
    worktree_patch = subprocess.run(
        ["git", "diff", "--binary", "HEAD"],
        cwd=REPOSITORY_ROOT,
        check=True,
        stdout=subprocess.PIPE,
    ).stdout

    with tempfile.TemporaryDirectory(prefix="cargo-rail-surface-reference-") as temporary:
        temporary_root = Path(temporary)
        reference_source = extract_archive(arguments.reference_archive, temporary_root / "reference-source")
        reference_binary = build_reference(reference_source, reference_authority["rust"], arguments.output)
        binaries = {"reference": reference_binary, "cargo-rail": rail_binary}
        identity = {
            "repository_commit": command_output(["git", "rev-parse", "HEAD"]),
            "worktree_patch_sha256": bytes_sha256(worktree_patch),
            "fixture_tree_sha256": tree_sha256(fixture),
            "cargo_rail_sha256": sha256(rail_binary),
            "cargo_rail_driver_sha256": sha256(rail_driver),
            "cargo_rail_version": command_output([str(rail_binary), "rail", "--version"], environment),
            "reference_version": command_output([str(reference_binary), "hawk", "--version"], environment),
            "rustc": command_output(["rustc", "-vV"], environment),
            "cargo": command_output(["cargo", "-V"], environment),
            "host": platform.uname()._asdict(),
            "python": platform.python_version(),
            "target": manifest["target"],
            "feature_profiles": manifest["feature_profiles"],
            "doctest_packages": manifest["doctest_packages"],
            "output_sink": "one JSON value on stdout",
        }
        (arguments.output / "identity.json").write_text(json.dumps(identity, indent=2) + "\n", encoding="utf-8")

        measurements: list[dict[str, Any]] = []
        if arguments.conformance_only:
            for case in cases:
                measurements.extend(
                    execute_pair(
                        case,
                        "cold-target",
                        0,
                        temporary_root / f"conformance-{case['name']}-workspaces",
                        temporary_root / f"conformance-{case['name']}-state",
                        arguments.output,
                        fixture,
                        binaries,
                        environment,
                        manifest,
                        False,
                    )
                )
            accepted = 1
            lanes = ["cold-target"]
        else:
            accepted = arguments.runs
            lanes = list(LANES)
            for lane in lanes:
                persistent = lane in ("cargo-fresh", "fact-cache-hit")
                execute_pair(
                    performance_case,
                    lane,
                    -1,
                    temporary_root / f"{lane}-workspaces",
                    temporary_root / f"{lane}-state",
                    arguments.output,
                    fixture,
                    binaries,
                    environment,
                    manifest,
                    persistent,
                )
            for index in range(accepted):
                for lane in LANES[index % len(LANES) :] + LANES[: index % len(LANES)]:
                    measurements.extend(
                        execute_pair(
                            performance_case,
                            lane,
                            index,
                            temporary_root / f"{lane}-workspaces",
                            temporary_root / f"{lane}-state",
                            arguments.output,
                            fixture,
                            binaries,
                            environment,
                            manifest,
                            lane in ("cargo-fresh", "fact-cache-hit"),
                        )
                    )
            validate_lane(measurements)

        summary: dict[str, Any] = {
            "schema_version": 2,
            "reference": reference_authority,
            "identity": identity,
            "fixture": manifest["fixture"],
            "cases": case_names,
            "lanes": lanes,
            "accepted_runs_per_lane": accepted,
            "measurements": measurements,
            "statistics": {},
        }
        for lane in lanes:
            summary["statistics"][lane] = {}
            for analyzer in ("reference", "cargo-rail"):
                selected = [
                    item["elapsed_seconds"]
                    for item in measurements
                    if item["lane"] == lane and item["analyzer"] == analyzer
                ]
                summary["statistics"][lane][analyzer] = {
                    "p50_seconds": statistics.median(selected),
                    "p95_seconds": percentile(selected, 0.95),
                }
        (arguments.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.CalledProcessError, QualificationError) as error:
        print(f"surface reference qualification: {error}", file=sys.stderr)
        sys.exit(1)
