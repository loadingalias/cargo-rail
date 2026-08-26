#!/usr/bin/env python3
"""Qualify shared compiler-fact acquisition against executed independent work."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MEASURE_COMMAND = REPOSITORY_ROOT / "scripts/bench/measure-command.py"
DRIVER_MANIFEST = REPOSITORY_ROOT / "tools/compiler-fact-driver/Cargo.toml"
TEST_NAME = "compiler::collector::tests::compiler_fact_acquisition_qualification_sample"
MINIMUM_QUALIFICATION_SAMPLES = 20
MINIMUM_COMBINED_REDUCTION_PERCENT = 10.0
MAXIMUM_WARM_FRACTION = 0.10
COMBINED_CARGO_VIEWS = 3
INDEPENDENT_CARGO_VIEWS = 4
SCRUBBED_ENVIRONMENT = (
    "CARGO_BUILD_JOBS",
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


class QualificationError(RuntimeError):
    """The compiler-fact evidence does not satisfy its qualification contract."""


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    subcommands = value.add_subparsers(dest="operation", required=True)
    subcommands.add_parser("smoke")
    run = subcommands.add_parser("run")
    run.add_argument("runs", type=int)
    for operation in ("summarize", "validate"):
        command = subcommands.add_parser(operation)
        command.add_argument("results", type=Path)
    return value


def run_command(
    argv: list[str],
    *,
    env: dict[str, str],
    stdout: Path | None = None,
    stderr: Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(argv, cwd=REPOSITORY_ROOT, env=env, capture_output=True, check=False)
    if stdout is not None:
        stdout.write_bytes(completed.stdout)
    if stderr is not None:
        stderr.write_bytes(completed.stderr)
    if completed.returncode != 0:
        raise QualificationError(
            f"command exited {completed.returncode}: {subprocess.list2cmdline(argv)}\n"
            f"stdout:\n{completed.stdout.decode(errors='replace')}\n"
            f"stderr:\n{completed.stderr.decode(errors='replace')}"
        )
    return completed


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(128 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def command_text(argv: list[str], env: dict[str, str]) -> str:
    return run_command(argv, env=env).stdout.decode("utf-8").strip()


def rustc_identity(env: dict[str, str]) -> dict[str, str]:
    verbose = command_text(["rustc", "-vV"], env)
    fields = {}
    for line in verbose.splitlines():
        name, separator, value = line.partition(": ")
        if separator:
            fields[name] = value
    for required in ("release", "commit-hash", "host"):
        if not fields.get(required):
            raise QualificationError(f"rustc -vV omitted {required}")
    return {
        "verbose": verbose,
        "release": fields["release"],
        "commit": fields["commit-hash"],
        "host": fields["host"],
    }


def compiler_library(sysroot: Path) -> Path:
    candidates = []
    for directory in (sysroot / "lib", sysroot / "bin"):
        if not directory.is_dir():
            continue
        for pattern in ("librustc_driver-*.so", "librustc_driver-*.dylib", "rustc_driver-*.dll"):
            candidates.extend(path for path in directory.glob(pattern) if path.is_file())
    if len(candidates) != 1:
        raise QualificationError(
            f"expected exactly one rustc_driver runtime library under {sysroot}, found {len(candidates)}"
        )
    return candidates[0].resolve()


def driver_provenance() -> str:
    files = [
        REPOSITORY_ROOT / "tools/compiler-fact-driver/Cargo.lock",
        REPOSITORY_ROOT / "tools/compiler-fact-driver/Cargo.toml",
        REPOSITORY_ROOT / "tools/compiler-fact-driver/build.rs",
        *sorted((REPOSITORY_ROOT / "tools/compiler-fact-driver/src").glob("*.rs")),
        REPOSITORY_ROOT / "src/compiler/fact_protocol.rs",
    ]
    framed = b"".join(
        f"{sha256_file(path)}  {path.relative_to(REPOSITORY_ROOT).as_posix()}\n".encode("utf-8")
        for path in files
    )
    return sha256_bytes(framed)


def authoritative_environment(setup: Path) -> tuple[dict[str, str], dict[str, Any]]:
    environment = os.environ.copy()
    for name in SCRUBBED_ENVIRONMENT:
        environment.pop(name, None)
    environment.update(
        {
            "CARGO_INCREMENTAL": "0",
            "CARGO_TERM_COLOR": "never",
            "RUSTUP_AUTO_INSTALL": "0",
            "RUSTUP_NO_UPDATE_CHECK": "1",
        }
    )
    for tool in ("cargo", "git", "rustc", "rustup"):
        if shutil.which(tool, path=environment.get("PATH")) is None:
            raise QualificationError(f"missing required qualification tool: {tool}")

    rustc = rustc_identity(environment)
    sysroot = Path(command_text(["rustc", "--print", "sysroot"], environment)).resolve()
    runtime = compiler_library(sysroot)
    build_environment = environment | {"RUSTC_BOOTSTRAP": "cargo_rail_fact_driver"}
    run_command(
        ["cargo", "build", "--locked", "--release", "--manifest-path", str(DRIVER_MANIFEST)],
        env=build_environment,
        stdout=setup / "driver-build.stdout",
        stderr=setup / "driver-build.stderr",
    )
    executable_suffix = ".exe" if os.name == "nt" else ""
    driver = REPOSITORY_ROOT / f"tools/compiler-fact-driver/target/release/cargo-rail-fact-driver{executable_suffix}"
    if not driver.is_file():
        raise QualificationError(f"compiler-fact driver build omitted {driver}")
    relative_runtime = runtime.relative_to(sysroot).as_posix()
    authority = {
        "CARGO_RAIL_FACT_DRIVER_FILE": driver.name,
        "CARGO_RAIL_FACT_DRIVER_SHA256": f"sha256:{sha256_file(driver)}",
        "CARGO_RAIL_FACT_DRIVER_PROVENANCE": f"sha256:{driver_provenance()}",
        "CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE": rustc["release"],
        "CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT": rustc["commit"],
        "CARGO_RAIL_FACT_DRIVER_RUSTC_HOST": rustc["host"],
        "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY": relative_runtime,
        "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256": f"sha256:{sha256_file(runtime)}",
    }
    environment.update(authority)

    run_command(
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "--package",
            "cargo-rail",
            "--all-features",
            "--bin",
            "cargo-rail",
            "--bin",
            "cargo-rail-compiler-observation",
        ],
        env=environment,
        stdout=setup / "cargo-rail-build.stdout",
        stderr=setup / "cargo-rail-build.stderr",
    )
    cargo_rail = REPOSITORY_ROOT / f"target/release/cargo-rail{executable_suffix}"
    observation = REPOSITORY_ROOT / f"target/release/cargo-rail-compiler-observation{executable_suffix}"
    if not cargo_rail.is_file():
        raise QualificationError(f"cargo-rail release build omitted {cargo_rail}")
    if not observation.is_file():
        raise QualificationError(f"cargo-rail release build omitted {observation}")

    listing = run_command(
        [
            "cargo",
            "nextest",
            "list",
            "--lib",
            "--package",
            "cargo-rail",
            "--all-features",
            "--locked",
            "--release",
            "--message-format",
            "json",
            "--run-ignored",
            "only",
            TEST_NAME,
        ],
        env=environment,
        stdout=setup / "nextest-list.json",
        stderr=setup / "nextest-list.stderr",
    )
    try:
        suites = json.loads(listing.stdout)["rust-suites"]
        matches = [
            Path(suite["binary-path"])
            for suite in suites.values()
            if TEST_NAME in suite.get("testcases", {})
        ]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise QualificationError(f"nextest returned an invalid test inventory: {error}") from error
    if len(matches) != 1 or not matches[0].is_file():
        raise QualificationError(f"nextest resolved {len(matches)} qualification test binaries")
    test_binary = matches[0].resolve()
    environment.update(
        {
            "CARGO_RAIL_TEST_FACT_DRIVER": str(driver.resolve()),
            "CARGO_RAIL_TEST_OBSERVATION_WRAPPER": str(observation.resolve()),
        }
    )
    metadata = {
        "rustc": rustc,
        "cargo": command_text(["cargo", "-Vv"], environment),
        "driver": str(driver.resolve()),
        "driver_sha256": sha256_file(driver),
        "driver_bytes": driver.stat().st_size,
        "driver_provenance": authority["CARGO_RAIL_FACT_DRIVER_PROVENANCE"],
        "compiler_library": relative_runtime,
        "compiler_library_sha256": sha256_file(runtime),
        "cargo_rail": str(cargo_rail.resolve()),
        "cargo_rail_sha256": sha256_file(cargo_rail),
        "observation": str(observation.resolve()),
        "observation_sha256": sha256_file(observation),
        "cargo_rail_bytes": cargo_rail.stat().st_size,
        "test_binary": str(test_binary),
        "test_binary_sha256": sha256_file(test_binary),
    }
    return environment, metadata


def extract_result(stdout: bytes) -> dict[str, Any]:
    matches = []
    for raw_line in stdout.decode("utf-8", errors="replace").splitlines():
        line = raw_line.strip()
        if not line.startswith("{"):
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if value.get("workload") == "compiler-fact-acquisition":
            matches.append(value)
    if len(matches) != 1:
        raise QualificationError(f"qualification test emitted {len(matches)} workload records")
    return matches[0]


def measure_sample(
    results: Path,
    environment: dict[str, str],
    test_binary: Path,
    round_number: int,
    order: int,
    lane: str,
) -> dict[str, Any]:
    directory = results / "raw" / f"{round_number:03d}-{order}-{lane}"
    directory.mkdir(parents=True)
    stdout = directory / "stdout"
    stderr = directory / "stderr"
    timing = directory / "timing.json"
    with tempfile.TemporaryDirectory(prefix=f"cargo-rail-facts-{lane}-") as temporary:
        sample_environment = environment | {
            "CARGO_BUILD_JOBS": "1",
            "CARGO_RAIL_CACHE_DIR": str(Path(temporary) / "cache"),
            "CARGO_RAIL_CACHE_MAX_BYTES": str(1024 * 1024 * 1024),
            "CARGO_RAIL_COMPILER_FACT_QUALIFICATION_LANE": lane,
        }
        measured = subprocess.run(
            [
                sys.executable,
                str(MEASURE_COMMAND),
                "--cwd",
                str(REPOSITORY_ROOT),
                "--stdout",
                str(stdout),
                "--stderr",
                str(stderr),
                "--output",
                str(timing),
                "--",
                str(test_binary),
                TEST_NAME,
                "--exact",
                "--ignored",
                "--nocapture",
            ],
            cwd=REPOSITORY_ROOT,
            env=sample_environment,
            check=False,
        )
    measurement = json.loads(timing.read_text(encoding="utf-8"))
    sample: dict[str, Any] = {
        "schema_version": 2,
        "round": round_number,
        "order": order,
        "lane": lane,
        "accepted": False,
        "measurement": measurement,
        "result": None,
        "rejection": None,
    }
    try:
        if measured.returncode != 0 or measurement["exit_code"] != 0:
            raise QualificationError(f"test process exited {measurement['exit_code']}")
        payload = extract_result(stdout.read_bytes())
        if payload.get("schema_version") != 2 or payload.get("lane") != lane:
            raise QualificationError("test workload record does not match the requested lane")
        expected_cargo_views = COMBINED_CARGO_VIEWS if lane == "combined" else INDEPENDENT_CARGO_VIEWS
        if payload.get("cold_cargo_views") != expected_cargo_views:
            raise QualificationError("test workload executed the wrong number of Cargo views")
        if lane == "combined" and (
            payload.get("warm_cargo_views") != 0 or payload.get("warm_compiler_invocations") != 0
        ):
            raise QualificationError("warm exact reuse executed compiler work")
        sample["accepted"] = True
        sample["result"] = payload
    except (QualificationError, KeyError, TypeError, json.JSONDecodeError) as error:
        sample["rejection"] = str(error)
    (directory / "sample.json").write_text(json.dumps(sample, indent=2) + "\n", encoding="utf-8")
    return sample


def quantile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[int((len(ordered) - 1) * fraction)]


def metric(samples: list[dict[str, Any]], lane: str, field: str) -> dict[str, Any]:
    values = [sample["result"][field] / 1_000_000_000 for sample in samples if sample["lane"] == lane]
    return {
        "lane": lane,
        "accepted_samples": len(values),
        "p50_seconds": quantile(values, 0.50),
        "p95_seconds": quantile(values, 0.95),
        "minimum_seconds": min(values) if values else None,
        "maximum_seconds": max(values) if values else None,
        "mean_seconds": sum(values) / len(values) if values else None,
    }


def reduction(baseline: float | None, candidate: float | None) -> float | None:
    if baseline in (None, 0) or candidate is None:
        return None
    return 100.0 * (baseline - candidate) / baseline


def summarize(results: Path) -> dict[str, Any]:
    results = results.resolve()
    run_contract = json.loads((results / "run.json").read_text(encoding="utf-8"))
    environment = json.loads((results / "environment.json").read_text(encoding="utf-8"))
    samples = [
        json.loads(path.read_text(encoding="utf-8"))
        for path in sorted((results / "raw").glob("*/sample.json"))
    ]
    accepted = [sample for sample in samples if sample.get("accepted")]
    independent = metric(accepted, "independent", "cold_wall_ns")
    combined = metric(accepted, "combined", "cold_wall_ns")
    warm = metric(accepted, "combined", "warm_wall_ns")
    paired = {}
    for sample in accepted:
        paired.setdefault(sample["round"], {})[sample["lane"]] = sample
    pair_contracts = []
    for round_number, lanes in sorted(paired.items()):
        if set(lanes) != {"combined", "independent"}:
            continue
        combined_result = lanes["combined"]["result"]
        independent_result = lanes["independent"]["result"]
        pair_contracts.append(
            {
                "round": round_number,
                "exact_facts_equal": (
                    combined_result["exact_fact_set_digest"] == independent_result["exact_fact_set_digest"]
                    and combined_result["exact_fact_identities"] == independent_result["exact_fact_identities"]
                    and combined_result["exact_fact_bytes"] == independent_result["exact_fact_bytes"]
                ),
                "combined_compiler_invocations": combined_result["cold_compiler_invocations"],
                "independent_compiler_invocations": independent_result["cold_compiler_invocations"],
                "compiler_work_reduced": (
                    combined_result["cold_compiler_invocations"]
                    < independent_result["cold_compiler_invocations"]
                ),
            }
        )
    combined_p50_reduction = reduction(independent["p50_seconds"], combined["p50_seconds"])
    combined_p95_reduction = reduction(independent["p95_seconds"], combined["p95_seconds"])
    warm_p50_fraction = (
        None if combined["p50_seconds"] in (None, 0) else warm["p50_seconds"] / combined["p50_seconds"]
    )
    warm_p95_fraction = (
        None if combined["p95_seconds"] in (None, 0) else warm["p95_seconds"] / combined["p95_seconds"]
    )
    required = run_contract["required_accepted_samples_per_lane"]
    mechanism_valid = (
        len(samples) == required * 2
        and len(accepted) == len(samples)
        and independent["accepted_samples"] == required
        and combined["accepted_samples"] == required
        and len(pair_contracts) == required
        and all(pair["exact_facts_equal"] and pair["compiler_work_reduced"] for pair in pair_contracts)
    )
    performance_qualified = (
        run_contract["evidence_kind"] == "retained"
        and required >= MINIMUM_QUALIFICATION_SAMPLES
        and mechanism_valid
        and combined_p50_reduction is not None
        and combined_p50_reduction >= MINIMUM_COMBINED_REDUCTION_PERCENT
        and combined_p95_reduction is not None
        and combined_p95_reduction >= MINIMUM_COMBINED_REDUCTION_PERCENT
        and warm_p50_fraction is not None
        and warm_p50_fraction <= MAXIMUM_WARM_FRACTION
        and warm_p95_fraction is not None
        and warm_p95_fraction <= MAXIMUM_WARM_FRACTION
    )
    summary = {
        "schema_version": 2,
        "evidence_kind": run_contract["evidence_kind"],
        "host": environment["rustc"]["host"],
        "requested_samples_per_lane": required,
        "total_samples": len(samples),
        "accepted_samples": len(accepted),
        "rejected_samples": len(samples) - len(accepted),
        "metrics": [independent, combined, warm | {"lane": "warm-exact-reuse"}],
        "combined_p50_reduction_percent": combined_p50_reduction,
        "combined_p95_reduction_percent": combined_p95_reduction,
        "warm_p50_fraction_of_cold": warm_p50_fraction,
        "warm_p95_fraction_of_cold": warm_p95_fraction,
        "pair_contracts": pair_contracts,
        "mechanism_valid": mechanism_valid,
        "performance_qualified": performance_qualified,
        "contract": run_contract,
        "environment": environment,
    }
    (results / "samples.jsonl").write_text(
        "".join(json.dumps(sample, separators=(",", ":")) + "\n" for sample in samples),
        encoding="utf-8",
    )
    (results / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return summary


def validate(summary: dict[str, Any]) -> None:
    if not summary["mechanism_valid"]:
        raise QualificationError("compiler-fact samples did not preserve the exact work and object contracts")
    if summary["evidence_kind"] == "retained" and not summary["performance_qualified"]:
        raise QualificationError("compiler-fact retained run did not satisfy the performance contract")


def capture_worktree(results: Path, environment: dict[str, str]) -> dict[str, Any]:
    commit = command_text(["git", "rev-parse", "HEAD"], environment)
    status = run_command(["git", "status", "--porcelain=v1", "--untracked-files=all"], env=environment).stdout
    diff = run_command(["git", "diff", "--binary", "HEAD", "--"], env=environment).stdout
    (results / "worktree-status.txt").write_bytes(status)
    (results / "worktree.diff").write_bytes(diff)
    untracked = run_command(["git", "ls-files", "--others", "--exclude-standard", "-z"], env=environment).stdout
    entries = []
    for raw_path in untracked.split(b"\0"):
        if not raw_path:
            continue
        path = REPOSITORY_ROOT / os.fsdecode(raw_path)
        if not path.is_file():
            raise QualificationError(f"cannot identify non-file untracked input: {path}")
        entries.append({"path": path.relative_to(REPOSITORY_ROOT).as_posix(), "sha256": sha256_file(path)})
    (results / "untracked.json").write_text(json.dumps(entries, indent=2) + "\n", encoding="utf-8")
    return {
        "repository_commit": commit,
        "worktree_status_sha256": sha256_bytes(status),
        "worktree_diff_sha256": sha256_bytes(diff),
        "untracked_sha256": sha256_file(results / "untracked.json"),
    }


def run_qualification(runs: int, evidence_kind: str) -> Path:
    if runs <= 0:
        raise QualificationError("compiler-fact qualification runs must be positive")
    default_root = REPOSITORY_ROOT / "target/benchmarks/compiler-facts"
    timestamp = __import__("datetime").datetime.now(__import__("datetime").timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    results = Path(
        os.environ.get("CARGO_RAIL_COMPILER_FACT_RESULTS", str(default_root / f"{timestamp}-{evidence_kind}"))
    ).resolve()
    if results.exists():
        if "CARGO_RAIL_COMPILER_FACT_RESULTS" not in os.environ:
            raise QualificationError(f"compiler-fact result directory already exists: {results}")
        unexpected = [path.name for path in results.iterdir() if path.name != "correctness.log"]
        if unexpected:
            raise QualificationError(
                f"compiler-fact result directory contains unexpected entries: {', '.join(sorted(unexpected))}"
            )
    else:
        results.mkdir(parents=True)
    (results / "setup").mkdir()
    (results / "raw").mkdir()
    lock = REPOSITORY_ROOT / "target/benchmarks/.compiler-facts.lock"
    try:
        lock.mkdir(parents=True)
    except FileExistsError as error:
        raise QualificationError(f"compiler-fact qualification is already running: {lock}") from error
    try:
        environment, binaries = authoritative_environment(results / "setup")
        source_identity = capture_worktree(results, environment)
        harness_files = [
            MEASURE_COMMAND,
            Path(__file__).resolve(),
            REPOSITORY_ROOT / "src/compiler/collector.rs",
            REPOSITORY_ROOT / "scripts/build-compiler-fact-driver.sh",
        ]
        harness_manifest = "".join(
            f"{sha256_file(path)}  {path.relative_to(REPOSITORY_ROOT).as_posix()}\n" for path in harness_files
        )
        (results / "harness-sha256.txt").write_text(harness_manifest, encoding="utf-8")
        product_evidence = sys.platform.startswith("linux") or sys.platform == "win32"
        environment_record = {
            "schema_version": 2,
            **source_identity,
            **binaries,
            "benchmark_harness_sha256": sha256_file(results / "harness-sha256.txt"),
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "python": sys.version,
            "dev_machine_target": os.environ.get("DEV_MACHINE_TARGET"),
            "dev_machine_instance_type": os.environ.get("DEV_MACHINE_INSTANCE_TYPE"),
            "product_evidence": product_evidence,
        }
        (results / "environment.json").write_text(json.dumps(environment_record, indent=2) + "\n", encoding="utf-8")
        contract = {
            "schema_version": 2,
            "evidence_kind": evidence_kind,
            "required_accepted_samples_per_lane": runs,
            "lanes": ["combined", "independent"],
            "interleaving": "alternating lane order by round; no sample exclusion",
            "qualification": {
                "minimum_samples_per_lane": MINIMUM_QUALIFICATION_SAMPLES,
                "combined_p50_and_p95_reduction_min_percent": MINIMUM_COMBINED_REDUCTION_PERCENT,
                "warm_p50_and_p95_maximum_fraction_of_combined_cold": MAXIMUM_WARM_FRACTION,
                "combined_cargo_views": COMBINED_CARGO_VIEWS,
                "independent_cargo_views": INDEPENDENT_CARGO_VIEWS,
                "warm_cargo_views": 0,
                "warm_compiler_invocations": 0,
            },
        }
        (results / "run.json").write_text(json.dumps(contract, indent=2) + "\n", encoding="utf-8")
        test_binary = Path(binaries["test_binary"])
        for round_number in range(1, runs + 1):
            lanes = ("independent", "combined") if round_number % 2 else ("combined", "independent")
            for order, lane in enumerate(lanes, start=1):
                sample = measure_sample(results, environment, test_binary, round_number, order, lane)
                if not sample["accepted"]:
                    summarize(results)
                    raise QualificationError(
                        f"round {round_number} {lane} was rejected: {sample['rejection']}"
                    )
                payload = sample["result"]
                print(
                    f"compiler-facts: round={round_number}/{runs} lane={lane} "
                    f"cold={payload['cold_wall_ns'] / 1_000_000_000:.3f}s "
                    f"views={payload['cold_cargo_views']} invocations={payload['cold_compiler_invocations']}",
                    flush=True,
                )
        summary = summarize(results)
        validate(summary)
        print(
            json.dumps(
                {
                    "results": str(results),
                    "host": summary["host"],
                    "performance_qualified": summary["performance_qualified"],
                    "combined_p50_reduction_percent": summary["combined_p50_reduction_percent"],
                    "combined_p95_reduction_percent": summary["combined_p95_reduction_percent"],
                    "warm_p95_fraction_of_cold": summary["warm_p95_fraction_of_cold"],
                },
                indent=2,
            )
        )
        return results
    finally:
        shutil.rmtree(lock, ignore_errors=True)


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.operation == "smoke":
            run_qualification(1, "smoke")
        elif arguments.operation == "run":
            run_qualification(arguments.runs, "retained")
        else:
            summary = summarize(arguments.results)
            if arguments.operation == "validate":
                validate(summary)
            print(
                json.dumps(
                    {
                        "host": summary["host"],
                        "requested_samples_per_lane": summary["requested_samples_per_lane"],
                        "accepted_samples": summary["accepted_samples"],
                        "rejected_samples": summary["rejected_samples"],
                        "mechanism_valid": summary["mechanism_valid"],
                        "performance_qualified": summary["performance_qualified"],
                        "combined_p50_reduction_percent": summary["combined_p50_reduction_percent"],
                        "combined_p95_reduction_percent": summary["combined_p95_reduction_percent"],
                        "warm_p95_fraction_of_cold": summary["warm_p95_fraction_of_cold"],
                    },
                    indent=2,
                )
            )
        return 0
    except (QualificationError, OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        print(f"compiler-facts: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
