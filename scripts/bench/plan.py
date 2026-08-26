#!/usr/bin/env python3
"""Measure planner latency and structural work on deterministic Git fixtures."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Callable


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
DIAGNOSTIC_SCHEMA_VERSION = 12
PLAN_CONTRACT_VERSION = 8
FULL_RUNS = 20
LARGE_DIFF_FILES = 256
LATENCY_BUDGET_NS = {
    "clean_worktree": {"p50": 150_000_000, "p95": 250_000_000},
    "one_rust_file": {"p95": 300_000_000},
    "one_markdown_file": {"p95": 300_000_000},
    "semantic_config": {"p95": 400_000_000},
    "semantic_manifest": {"p95": 400_000_000},
    "object_pair_warm_metadata": {"p95": 250_000_000},
}
GIT_SUBPROCESS_BUDGET = {
    "clean_worktree": 7,
    "one_rust_file": 9,
    "one_markdown_file": 9,
    "semantic_config": 11,
    "semantic_manifest": 11,
    "large_diff": 9,
    "object_pair_cold_metadata": 6,
    "object_pair_warm_metadata": 6,
}
CONSUMER_P95_BUDGET_NS = 25_000_000
COUNTERS = (
    "cargo_metadata_loads",
    "cargo_metadata_cache_hits",
    "target_view_loads",
    "hash_operations",
    "hash_input_bytes",
    "hashed_file_bytes_read",
    "git_subprocesses",
    "git_object_reads",
    "git_object_read_batches",
    "git_path_change_reads",
    "git_path_change_batches",
    "graph_traversals",
    "graph_node_visits",
    "graph_edge_visits",
)
SCRUBBED_ENVIRONMENT = (
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
)


class BenchmarkError(RuntimeError):
    """The planner benchmark could not produce trustworthy evidence."""


@dataclass(frozen=True)
class Fixture:
    root: Path
    plan_args: tuple[str, ...]
    expected_changed_files: int


@dataclass(frozen=True)
class Lane:
    name: str
    fixture: Fixture
    runner: str = "direct"
    warmup: bool = True
    metadata_authority: str = "fresh_snapshot"


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    subcommands = value.add_subparsers(dest="operation", required=True)
    subcommands.add_parser("smoke")
    run = subcommands.add_parser("run")
    run.add_argument("runs", type=int, nargs="?", default=FULL_RUNS)
    return value


def benchmark_environment() -> dict[str, str]:
    environment = os.environ.copy()
    for name in SCRUBBED_ENVIRONMENT:
        environment.pop(name, None)
    environment.update(
        {
            "CARGO_INCREMENTAL": "0",
            "CARGO_NET_OFFLINE": "true",
            "CARGO_TERM_COLOR": "never",
            "GIT_CONFIG_COUNT": "2",
            "GIT_CONFIG_GLOBAL": str(REPOSITORY_ROOT / "tests/fixtures/isolated.gitconfig"),
            "GIT_CONFIG_KEY_0": "commit.gpgsign",
            "GIT_CONFIG_KEY_1": "tag.gpgsign",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_VALUE_0": "false",
            "GIT_CONFIG_VALUE_1": "false",
            "GIT_TERMINAL_PROMPT": "0",
            "RUSTUP_AUTO_INSTALL": "0",
            "RUSTUP_NO_UPDATE_CHECK": "1",
        }
    )
    return environment


def run_command(
    argv: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(argv, cwd=cwd, env=environment, capture_output=True, check=False)
    if completed.returncode != 0:
        raise BenchmarkError(
            f"command exited {completed.returncode}: {subprocess.list2cmdline(argv)}\n"
            f"stdout:\n{completed.stdout.decode(errors='replace')}\n"
            f"stderr:\n{completed.stderr.decode(errors='replace')}"
        )
    return completed


def command_text(argv: list[str], environment: dict[str, str], cwd: Path = REPOSITORY_ROOT) -> str:
    return run_command(argv, cwd=cwd, environment=environment).stdout.decode("utf-8").strip()


def git(root: Path, environment: dict[str, str], *args: str) -> str:
    return command_text(["git", *args], environment, root)


def write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(contents, encoding="utf-8")


def create_fixture(
    parent: Path,
    name: str,
    environment: dict[str, str],
    mutate: Callable[[Path, dict[str, str]], int] | None = None,
    *,
    object_pair: bool = False,
) -> Fixture:
    root = parent / name
    root.mkdir()
    git(root, environment, "init", "--initial-branch=main")
    git(root, environment, "config", "user.name", "Planner Benchmark")
    git(root, environment, "config", "user.email", "planner-benchmark@example.invalid")

    write(
        root / "Cargo.toml",
        """[workspace]
members = ["crates/core", "crates/app"]
resolver = "2"

[workspace.package]
edition = "2024"
license = "MIT"
""",
    )
    write(
        root / "crates/core/Cargo.toml",
        """[package]
name = "fixture-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true
""",
    )
    write(root / "crates/core/src/lib.rs", "pub fn answer() -> u8 { 42 }\n")
    write(
        root / "crates/app/Cargo.toml",
        """[package]
name = "fixture-app"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
fixture-core = { path = "../core" }
""",
    )
    write(root / "crates/app/src/main.rs", "fn main() { println!(\"{}\", fixture_core::answer()); }\n")
    write(
        root / ".config/rail.toml",
        "[release]\nsemver_check = \"warn\"\n\n[surface]\nenabled = true\n",
    )
    write(root / ".gitignore", "target/\n")
    write(root / "README.md", "# Planner benchmark fixture\n")
    for index in range(LARGE_DIFF_FILES):
        write(root / f"docs/generated-{index:04d}.md", f"# Document {index}\n")
    run_command(["cargo", "generate-lockfile", "--offline"], cwd=root, environment=environment)
    git(root, environment, "add", ".")
    git(root, environment, "commit", "-m", "Establish planner benchmark fixture")
    base = git(root, environment, "rev-parse", "HEAD")

    if object_pair:
        write(root / "crates/core/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
        git(root, environment, "add", "crates/core/src/lib.rs")
        git(root, environment, "commit", "-m", "Change core for object comparison")
        head = git(root, environment, "rev-parse", "HEAD")
        return Fixture(root, ("--from", base, "--to", head), 1)

    expected_changed_files = mutate(root, environment) if mutate is not None else 0
    return Fixture(root, ("--since", "HEAD"), expected_changed_files)


def rust_change(root: Path, _environment: dict[str, str]) -> int:
    write(root / "crates/core/src/lib.rs", "pub fn answer() -> u8 { 43 }\n")
    return 1


def markdown_change(root: Path, _environment: dict[str, str]) -> int:
    write(root / "README.md", "# Planner benchmark fixture\n\nChanged documentation.\n")
    return 1


def config_change(root: Path, _environment: dict[str, str]) -> int:
    write(
        root / ".config/rail.toml",
        "[release]\nsemver_check = \"off\"\n\n[surface]\nenabled = true\n",
    )
    return 1


def manifest_change(root: Path, environment: dict[str, str]) -> int:
    path = root / "crates/core/Cargo.toml"
    write(path, path.read_text(encoding="utf-8").replace('version = "0.1.0"', 'version = "0.1.1"'))
    run_command(["cargo", "generate-lockfile", "--offline"], cwd=root, environment=environment)
    return 2


def large_diff(root: Path, _environment: dict[str, str]) -> int:
    for index in range(LARGE_DIFF_FILES):
        write(root / f"docs/generated-{index:04d}.md", f"# Changed document {index}\n")
    return LARGE_DIFF_FILES


def release_binary() -> Path:
    configured = os.environ.get("CARGO_RAIL_BIN")
    suffix = ".exe" if os.name == "nt" else ""
    binary = Path(configured) if configured else REPOSITORY_ROOT / f"target/release/cargo-rail{suffix}"
    if not binary.is_file():
        raise BenchmarkError(
            f"release binary does not exist: {binary}; run cargo build --release --locked --bin cargo-rail"
        )
    return binary.resolve()


def bootstrap_target_directory() -> Path:
    configured = Path(os.environ.get("RAIL_BOOTSTRAP_TARGET_DIR", "target/cargo-rail-bootstrap"))
    if not configured.is_absolute():
        configured = REPOSITORY_ROOT / configured
    return configured.resolve()


def command_for_lane(binary: Path, lane: Lane, diagnostics: Path) -> list[str]:
    plan = [
        "rail",
        "--diagnostics-file",
        str(diagnostics),
        "plan",
        *lane.fixture.plan_args,
        "--json",
    ]
    if lane.runner == "direct":
        return [str(binary), *plan]
    if lane.runner == "cargo_bootstrap":
        return [
            "cargo",
            "run",
            "--quiet",
            "--locked",
            "--target-dir",
            str(bootstrap_target_directory()),
            "--manifest-path",
            str(REPOSITORY_ROOT / "Cargo.toml"),
            "--",
            *plan,
        ]
    raise BenchmarkError(f"unknown runner: {lane.runner}")


def remove_metadata_cache(fixture: Fixture) -> None:
    state = fixture.root / "target/cargo-rail"
    if state.exists():
        shutil.rmtree(state)


def validate_diagnostics(diagnostics: dict[str, Any], lane: Lane) -> None:
    if diagnostics.get("schema_version") != DIAGNOSTIC_SCHEMA_VERSION:
        raise BenchmarkError(f"{lane.name}: unexpected diagnostic schema: {diagnostics.get('schema_version')}")
    phases = diagnostics.get("phases")
    if not isinstance(phases, dict):
        raise BenchmarkError(f"{lane.name}: diagnostics omitted phases")
    if phases.get("cli_pre_context_preparation", {}).get("invocations") != 1:
        raise BenchmarkError(f"{lane.name}: planner did not prepare exactly once")
    if phases.get("workspace_capture_cargo_metadata", {}).get("invocations") != 1:
        raise BenchmarkError(f"{lane.name}: planner did not capture metadata exactly once")
    loads = diagnostics.get("cargo_metadata_loads")
    hits = diagnostics.get("cargo_metadata_cache_hits")
    expected = {
        "fresh_snapshot": (1, 0),
        "workspace_cache_cold": (1, 0),
        "workspace_cache_warm": (0, 1),
    }.get(lane.metadata_authority)
    if expected is None:
        raise BenchmarkError(f"{lane.name}: unknown metadata authority: {lane.metadata_authority}")
    if (loads, hits) != expected:
        raise BenchmarkError(
            f"{lane.name}: metadata authority {lane.metadata_authority} expected "
            f"loads={expected[0]}, hits={expected[1]}; found loads={loads}, hits={hits}"
        )


def measure_once(
    binary: Path,
    lane: Lane,
    diagnostics: Path,
    environment: dict[str, str],
) -> tuple[dict[str, Any], bytes]:
    if lane.metadata_authority == "workspace_cache_cold":
        remove_metadata_cache(lane.fixture)
    started = time.perf_counter_ns()
    completed = run_command(
        command_for_lane(binary, lane, diagnostics),
        cwd=lane.fixture.root,
        environment=environment,
    )
    wall_ns = time.perf_counter_ns() - started
    try:
        plan = json.loads(completed.stdout)
        counters = json.loads(diagnostics.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkError(f"{lane.name}: invalid benchmark output: {error}") from error
    if plan.get("plan_contract_version") != PLAN_CONTRACT_VERSION:
        raise BenchmarkError(f"{lane.name}: unexpected plan contract: {plan.get('plan_contract_version')}")
    files = plan.get("changes", {}).get("files")
    if not isinstance(files, list) or len(files) != lane.fixture.expected_changed_files:
        raise BenchmarkError(
            f"{lane.name}: expected {lane.fixture.expected_changed_files} changed files, found "
            f"{len(files) if isinstance(files, list) else 'invalid'}"
        )
    validate_diagnostics(counters, lane)
    sample = {
        "wall_ns": wall_ns,
        "stdout_bytes": len(completed.stdout),
        "stdout_sha256": hashlib.sha256(completed.stdout).hexdigest(),
        "diagnostics": counters,
    }
    diagnostics.unlink()
    return sample, completed.stdout


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * fraction + 0.999999) - 1))
    return ordered[index]


def statistics(values: list[int]) -> dict[str, int]:
    return {
        "min": min(values),
        "p50": percentile(values, 0.50),
        "p95": percentile(values, 0.95),
        "max": max(values),
    }


def summarize(samples: list[dict[str, Any]]) -> dict[str, Any]:
    counters = {
        name: statistics([sample["diagnostics"][name] for sample in samples])
        for name in COUNTERS
    }
    phases = {}
    for name in samples[0]["diagnostics"]["phases"]:
        phases[name] = {
            "elapsed_ns": statistics(
                [sample["diagnostics"]["phases"][name]["elapsed_ns"] for sample in samples]
            ),
            "invocations": sorted(
                {sample["diagnostics"]["phases"][name]["invocations"] for sample in samples}
            ),
        }
    return {
        "wall_ns": statistics([sample["wall_ns"] for sample in samples]),
        "stdout_bytes": statistics([sample["stdout_bytes"] for sample in samples]),
        "counters": counters,
        "phases": phases,
    }


def measure_lane(
    binary: Path,
    lane: Lane,
    runs: int,
    diagnostics_root: Path,
    environment: dict[str, str],
) -> dict[str, Any]:
    if lane.warmup:
        warmup = diagnostics_root / f"{lane.name}-warmup.json"
        measure_once(binary, lane, warmup, environment)

    samples = []
    expected_stdout: bytes | None = None
    for index in range(runs):
        diagnostics = diagnostics_root / f"{lane.name}-{index:03d}.json"
        sample, stdout = measure_once(binary, lane, diagnostics, environment)
        if expected_stdout is not None and stdout != expected_stdout:
            raise BenchmarkError(f"{lane.name}: equivalent samples produced different plan JSON")
        expected_stdout = stdout
        samples.append(sample)
        print(
            f"plan benchmark: lane={lane.name} sample={index + 1}/{runs} "
            f"wall={sample['wall_ns'] / 1_000_000:.1f}ms",
            file=sys.stderr,
        )
    return {
        "runner": lane.runner,
        "metadata_authority": lane.metadata_authority,
        "prior_plan_warmup": lane.warmup,
        "comparison": "objects" if "--from" in lane.fixture.plan_args else "worktree",
        "changed_files": lane.fixture.expected_changed_files,
        "summary": summarize(samples),
        "samples": samples,
    }


def results_directory() -> Path:
    configured = os.environ.get("CARGO_RAIL_BENCH_RESULTS")
    if configured:
        result = Path(configured)
    else:
        timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
        result = REPOSITORY_ROOT / "target/benchmarks/plan" / timestamp
    result.mkdir(parents=True, exist_ok=False)
    return result.resolve()


def environment_evidence(binary: Path, environment: dict[str, str]) -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "cargo": command_text(["cargo", "-V"], environment),
        "rustc": command_text(["rustc", "-vV"], environment),
        "git": command_text(["git", "--version"], environment),
        "cargo_rail": command_text([str(binary), "rail", "--version"], environment),
        "cargo_rail_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "repository_head": command_text(["git", "rev-parse", "HEAD"], environment),
        "rustup_toolchain": environment["RUSTUP_TOOLCHAIN"],
        "bootstrap_target_directory": str(bootstrap_target_directory()),
    }


def measure_consumer(plan_bytes: bytes, root: Path, runs: int) -> dict[str, Any]:
    path = root / "consumer-plan.json"
    path.write_bytes(plan_bytes)
    reader_path = REPOSITORY_ROOT / "scripts/plan/read.py"
    spec = importlib.util.spec_from_file_location("cargo_rail_plan_reader", reader_path)
    if spec is None or spec.loader is None:
        raise BenchmarkError("cannot load the reference plan reader")
    reader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reader)
    reader.load_plan(path)
    samples = []
    for _ in range(runs):
        started = time.perf_counter_ns()
        plan = reader.load_plan(path)
        reader.cargo_args(plan, "cargo.build")
        samples.append(time.perf_counter_ns() - started)
    return {"wall_ns": statistics(samples), "operations": ["validate", "cargo.build selector lowering"]}


def enforce_budgets(lanes: dict[str, Any], consumer: dict[str, Any], runs: int) -> dict[str, Any]:
    for name, maximum in GIT_SUBPROCESS_BUDGET.items():
        actual = lanes[name]["summary"]["counters"]["git_subprocesses"]["max"]
        if actual > maximum:
            raise BenchmarkError(f"{name}: {actual} Git subprocesses exceeds total bound {maximum}")
    clean = lanes["clean_worktree"]["summary"]["counters"]
    if clean["hashed_file_bytes_read"]["max"] != 0:
        raise BenchmarkError("clean planning read or hashed tracked file contents")
    for name, lane in lanes.items():
        loads = lane["summary"]["counters"]["cargo_metadata_loads"]["max"]
        hits = lane["summary"]["counters"]["cargo_metadata_cache_hits"]["max"]
        if loads + hits != 1:
            raise BenchmarkError(f"{name}: planner did not use exactly one Cargo metadata authority")
    if consumer["wall_ns"]["p95"] > CONSUMER_P95_BUDGET_NS:
        raise BenchmarkError("in-process plan validation and selector lowering exceeds 25 ms p95")
    if runs >= FULL_RUNS:
        for name, limits in LATENCY_BUDGET_NS.items():
            for percentile_name, maximum in limits.items():
                actual = lanes[name]["summary"]["wall_ns"][percentile_name]
                if actual > maximum:
                    raise BenchmarkError(
                        f"{name}: {percentile_name} {actual / 1_000_000:.1f} ms exceeds "
                        f"{maximum / 1_000_000:.1f} ms"
                    )
    return {
        "qualified": runs >= FULL_RUNS,
        "latency": LATENCY_BUDGET_NS,
        "git_subprocesses_total": GIT_SUBPROCESS_BUDGET,
        "consumer_p95_ns": CONSUMER_P95_BUDGET_NS,
        "clean_hashed_file_bytes_read": 0,
        "metadata_authorities": 1,
    }


def benchmark(runs: int) -> Path:
    if runs <= 0:
        raise BenchmarkError("planner benchmark runs must be positive")
    binary = release_binary()
    environment = benchmark_environment()
    for tool in ("cargo", "git", "rustc", "rustup"):
        if shutil.which(tool, path=environment.get("PATH")) is None:
            raise BenchmarkError(f"missing required benchmark tool: {tool}")
    active_toolchain = command_text(["rustup", "show", "active-toolchain"], environment).split()
    if not active_toolchain:
        raise BenchmarkError("rustup did not report the repository toolchain")
    environment["RUSTUP_TOOLCHAIN"] = active_toolchain[0]

    with tempfile.TemporaryDirectory(prefix="cargo-rail-plan-bench-") as temporary:
        temporary_root = Path(temporary)
        end_to_end = create_fixture(temporary_root, "end-to-end", environment)
        git(end_to_end.root, environment, "branch", "origin/main")
        git(end_to_end.root, environment, "commit", "--allow-empty", "-m", "Benchmark merge-base head")
        fixtures = {
            "clean": create_fixture(temporary_root, "clean", environment),
            "rust": create_fixture(temporary_root, "rust", environment, rust_change),
            "markdown": create_fixture(temporary_root, "markdown", environment, markdown_change),
            "config": create_fixture(temporary_root, "config", environment, config_change),
            "manifest": create_fixture(temporary_root, "manifest", environment, manifest_change),
            "large": create_fixture(temporary_root, "large", environment, large_diff),
            "objects": create_fixture(temporary_root, "objects", environment, object_pair=True),
            "end_to_end": Fixture(end_to_end.root, (), 0),
        }
        lanes = (
            Lane("clean_worktree", fixtures["clean"]),
            Lane("one_rust_file", fixtures["rust"]),
            Lane("one_markdown_file", fixtures["markdown"]),
            Lane("semantic_config", fixtures["config"]),
            Lane("semantic_manifest", fixtures["manifest"]),
            Lane("large_diff", fixtures["large"]),
            Lane(
                "object_pair_cold_metadata",
                fixtures["objects"],
                warmup=False,
                metadata_authority="workspace_cache_cold",
            ),
            Lane(
                "object_pair_warm_metadata",
                fixtures["objects"],
                metadata_authority="workspace_cache_warm",
            ),
            Lane("end_to_end_just_plan", fixtures["end_to_end"], runner="cargo_bootstrap"),
        )
        diagnostics_root = temporary_root / "diagnostics"
        diagnostics_root.mkdir()
        measured = {}
        clean_plan = None
        for lane in lanes:
            measured[lane.name] = measure_lane(binary, lane, runs, diagnostics_root, environment)
            if lane.name == "clean_worktree":
                diagnostics = diagnostics_root / "consumer-source-plan.json"
                _, clean_plan = measure_once(binary, lane, diagnostics, environment)
        if clean_plan is None:
            raise BenchmarkError("clean planner lane produced no consumer fixture")
        consumer = measure_consumer(clean_plan, temporary_root, runs)
        budgets = enforce_budgets(measured, consumer, runs)

    result = {
        "schema_version": 1,
        "benchmark": "plan-v8-qualification",
        "recorded_at": datetime.now(UTC).isoformat(),
        "runs_per_lane": runs,
        "fixture": {
            "packages": 2,
            "large_diff_files": LARGE_DIFF_FILES,
        },
        "environment": environment_evidence(binary, environment),
        "lanes": measured,
        "consumer_validation": consumer,
        "budgets": budgets,
    }
    output = results_directory()
    summary = output / "results.json"
    summary.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return summary


def main() -> int:
    arguments = parser().parse_args()
    runs = 1 if arguments.operation == "smoke" else arguments.runs
    if arguments.operation == "run" and runs < FULL_RUNS:
        raise BenchmarkError(f"full planner qualification requires at least {FULL_RUNS} runs per lane")
    summary = benchmark(runs)
    print(summary)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(f"planner benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
