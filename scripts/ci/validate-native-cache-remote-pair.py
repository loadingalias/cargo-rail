#!/usr/bin/env python3
"""Validate one retained native-cache producer/consumer evidence pair."""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import re
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


WORKLOADS = ("check", "build", "test")
REQUIRED_COMPILER_CLASSES = (
    "binary",
    "build_script",
    "c_dynamic_library",
    "proc_macro_producer",
    "rust_dynamic_library",
    "rust_library",
    "static_library",
    "test",
)


class ValidationError(Exception):
    """One retained input does not prove the remote-sharing contract."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValidationError(message)


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValidationError(f"cannot read retained JSON {path}: {error}") from error
    require(isinstance(value, dict), f"retained JSON is not an object: {path}")
    return value


def require_object(value: Any, context: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{context} is not an object")
    return value


def require_string_list(value: Any, context: str) -> list[str]:
    require(
        isinstance(value, list) and all(isinstance(item, str) and item for item in value),
        f"{context} is not an array of nonempty strings",
    )
    return value


def require_string(value: Any, context: str) -> str:
    require(isinstance(value, str) and bool(value), f"{context} is not a nonempty string")
    return value


def require_digest(value: Any, context: str) -> str:
    value = require_string(value, context)
    require(re.fullmatch(r"[0-9a-f]{64}", value) is not None, f"{context} is not a SHA-256 digest")
    return value


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as error:
        raise ValidationError(f"cannot hash retained file {path}: {error}") from error
    return digest.hexdigest()


def retained_inputs(root: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    require(root.is_dir(), f"retained result directory does not exist: {root}")
    environment = load_object(root / "environment.json")
    result = load_object(root / "result.json")
    require(environment.get("schema_version") == 2, f"unsupported environment schema in {root}")
    require(result.get("schema_version") == 2, f"unsupported result schema in {root}")
    require(result.get("passed") is True, f"retained qualification did not pass: {root}")
    require_string(environment.get("phase"), f"{root} phase")
    require_string(environment.get("run_id"), f"{root} run_id")
    repository_commit = require_string(environment.get("repository_commit"), f"{root} repository_commit")
    require(
        re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", repository_commit) is not None,
        f"{root} repository_commit is not an exact Git object ID",
    )
    for field in (
        "worktree_diff_sha256",
        "worktree_status_sha256",
        "release_binary_sha256",
        "benchmark_harness_sha256",
    ):
        require_digest(environment.get(field), f"{root} {field}")
    require_string(environment.get("rustc"), f"{root} rustc")
    require_string(environment.get("cargo"), f"{root} cargo")
    remote = require_object(environment.get("remote"), f"{root} remote authority")
    require(
        remote.get("provider") in {"aws-s3", "azure-blob", "cloudflare-r2"},
        f"{root} remote provider is not a qualified provider",
    )
    authority = require_string(remote.get("authority"), f"{root} remote authority ID")
    require(
        re.fullmatch(r"remote-authority-v1-sha256-[0-9a-f]{64}", authority) is not None,
        f"{root} remote authority ID is malformed",
    )
    require(remote.get("mode") in {"read", "read-write"}, f"{root} remote mode is invalid")
    coverage = require_object(result.get("compiler_class_coverage"), f"{root} compiler_class_coverage")
    require(coverage.get("complete") is True, f"compiler-class coverage is incomplete: {root}")
    require(
        tuple(coverage.get("required", ())) == REQUIRED_COMPILER_CLASSES,
        f"compiler-class requirement drifted: {root}",
    )
    return environment, result


def action_counter(summary: dict[str, Any], field: str, context: str) -> collections.Counter[str]:
    return collections.Counter(require_string_list(summary.get(field), f"{context}.{field}"))


def validate_pair(producer_root: Path, consumer_root: Path) -> dict[str, Any]:
    producer_environment, producer = retained_inputs(producer_root)
    consumer_environment, consumer = retained_inputs(consumer_root)
    require(producer_environment.get("phase") == "producer", "first result is not producer evidence")
    require(consumer_environment.get("phase") == "consumer", "second result is not consumer evidence")
    require(producer.get("phase") == "producer", "producer result phase disagrees with its environment")
    require(consumer.get("phase") == "consumer", "consumer result phase disagrees with its environment")

    for field in (
        "run_id",
        "repository_commit",
        "worktree_diff_sha256",
        "worktree_status_sha256",
        "release_binary_sha256",
        "benchmark_harness_sha256",
        "rustc",
        "cargo",
    ):
        require(
            producer_environment.get(field) == consumer_environment.get(field),
            f"producer and consumer disagree on {field}",
        )
    require(
        producer_environment.get("run_id") == producer_root.name.removesuffix("-producer"),
        "producer directory name does not bind its run ID",
    )
    require(
        consumer_environment.get("run_id") == consumer_root.name.removesuffix("-consumer"),
        "consumer directory name does not bind its run ID",
    )

    producer_remote = require_object(producer_environment.get("remote"), "producer remote authority")
    consumer_remote = require_object(consumer_environment.get("remote"), "consumer remote authority")
    for field in ("provider", "authority"):
        require(producer_remote.get(field) == consumer_remote.get(field), f"remote {field} differs across roots")
    require(producer_remote.get("mode") == "read-write", "producer remote mode is not read-write")
    require(consumer_remote.get("mode") == "read", "consumer remote mode is not read-only")

    producer_workloads = require_object(producer.get("workloads"), "producer workloads")
    consumer_workloads = require_object(consumer.get("workloads"), "consumer workloads")
    require(set(producer_workloads) == set(WORKLOADS), "producer workload set is not exact")
    require(set(consumer_workloads) == set(WORKLOADS), "consumer workload set is not exact")
    require(
        producer.get("compiler_class_coverage") == consumer.get("compiler_class_coverage"),
        "producer and consumer compiler-class coverage differs",
    )

    workload_evidence: dict[str, Any] = {}
    hashed_inputs: dict[str, str] = {
        "producer/environment.json": sha256(producer_root / "environment.json"),
        "producer/result.json": sha256(producer_root / "result.json"),
        "consumer/environment.json": sha256(consumer_root / "environment.json"),
        "consumer/result.json": sha256(consumer_root / "result.json"),
    }
    for workload in WORKLOADS:
        producer_entry = require_object(producer_workloads[workload], f"producer {workload}")
        consumer_entry = require_object(consumer_workloads[workload], f"consumer {workload}")
        producer_primary = require_object(producer_entry.get("primary"), f"producer {workload} primary")
        consumer_primary = require_object(consumer_entry.get("primary"), f"consumer {workload} primary")
        consumer_offline = require_object(consumer_entry.get("l1_offline"), f"consumer {workload} L1 offline")
        require(producer_entry.get("l1_offline") is None, f"producer {workload} unexpectedly has L1 evidence")

        published = action_counter(producer_primary, "published_action_ids", f"producer {workload}")
        imported = action_counter(consumer_primary, "remote_hit_action_ids", f"consumer {workload}")
        offline = action_counter(consumer_offline, "local_hit_action_ids", f"consumer {workload} L1 offline")
        require(published, f"producer {workload} published no actions")
        require(published == imported, f"producer/consumer action multiset differs for {workload}")
        require(
            all(offline[action] >= count for action, count in imported.items()),
            f"offline L1 does not contain every imported action for {workload}",
        )
        require(
            consumer_primary.get("remote_payload_bytes_written") == 0,
            f"read-only consumer wrote remote bytes for {workload}",
        )
        require(
            consumer_offline.get("hit_remote_request_attempts") == 0
            and consumer_offline.get("hit_remote_coordinator_requests") == 0
            and consumer_offline.get("remote_payload_bytes_read") == 0
            and consumer_offline.get("remote_payload_bytes_written") == 0,
            f"offline L1 performed remote work for {workload}",
        )

        output_paths = {
            "producer": producer_root / "raw" / workload / "producer" / "outputs.sha256",
            "consumer": consumer_root / "raw" / workload / "consumer" / "outputs.sha256",
            "consumer-l1-offline": consumer_root / "raw" / workload / "l1-offline" / "outputs.sha256",
        }
        output_hashes = {name: sha256(path) for name, path in output_paths.items()}
        require(len(set(output_hashes.values())) == 1, f"exact output manifest differs for {workload}")
        hashed_inputs.update({f"{workload}/{name}/outputs.sha256": value for name, value in output_hashes.items()})
        workload_evidence[workload] = {
            "published_actions": sum(published.values()),
            "imported_actions": sum(imported.values()),
            "offline_l1_actions": sum(offline.values()),
            "exact_output_manifest_sha256": output_hashes["producer"],
        }

    return {
        "schema_version": 1,
        "generated_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "run_id": producer_environment["run_id"],
        "remote": {"provider": producer_remote["provider"], "authority": producer_remote["authority"]},
        "root_independent_action_multisets_equal": True,
        "exact_outputs_equal": True,
        "read_only_consumer": True,
        "offline_l1_without_remote_work": True,
        "compiler_class_coverage": producer["compiler_class_coverage"],
        "workloads": workload_evidence,
        "retained_input_sha256": dict(sorted(hashed_inputs.items())),
        "passed": True,
    }


def write_fixture(root: Path, phase: str) -> None:
    remote_mode = "read-write" if phase == "producer" else "read"
    environment = {
        "schema_version": 2,
        "phase": phase,
        "run_id": "fixture",
        "repository_commit": "a" * 40,
        "worktree_diff_sha256": "b" * 64,
        "worktree_status_sha256": "c" * 64,
        "release_binary_sha256": "d" * 64,
        "benchmark_harness_sha256": "e" * 64,
        "rustc": "rustc fixture",
        "cargo": "cargo fixture",
        "remote": {
            "provider": "aws-s3",
            "authority": f"remote-authority-v1-sha256-{'f' * 64}",
            "mode": remote_mode,
        },
    }
    workloads: dict[str, Any] = {}
    for workload in WORKLOADS:
        action = f"{workload}-action"
        primary = {
            "published_action_ids": [action] if phase == "producer" else [],
            "remote_hit_action_ids": [action] if phase == "consumer" else [],
            "local_hit_action_ids": [],
            "remote_payload_bytes_written": 1 if phase == "producer" else 0,
        }
        offline = None
        if phase == "consumer":
            offline = {
                "local_hit_action_ids": [action],
                "hit_remote_request_attempts": 0,
                "hit_remote_coordinator_requests": 0,
                "remote_payload_bytes_read": 0,
                "remote_payload_bytes_written": 0,
            }
        workloads[workload] = {"primary": primary, "l1_offline": offline}
        for label in ([phase] if phase == "producer" else [phase, "l1-offline"]):
            output = root / "raw" / workload / label / "outputs.sha256"
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(f"fixture  {workload}\n", encoding="utf-8")
    result = {
        "schema_version": 2,
        "phase": phase,
        "workloads": workloads,
        "compiler_class_coverage": {
            "complete": True,
            "required": list(REQUIRED_COMPILER_CLASSES),
            "observed": list(REQUIRED_COMPILER_CLASSES),
        },
        "passed": True,
    }
    (root / "environment.json").write_text(json.dumps(environment), encoding="utf-8")
    (root / "result.json").write_text(json.dumps(result), encoding="utf-8")


def self_test() -> None:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        producer = root / "fixture-producer"
        consumer = root / "fixture-consumer"
        producer.mkdir()
        consumer.mkdir()
        write_fixture(producer, "producer")
        write_fixture(consumer, "consumer")
        require(validate_pair(producer, consumer)["passed"] is True, "passing fixture did not validate")
        result = load_object(consumer / "result.json")
        result["workloads"]["check"]["primary"]["remote_hit_action_ids"] = []
        (consumer / "result.json").write_text(json.dumps(result), encoding="utf-8")
        try:
            validate_pair(producer, consumer)
        except ValidationError:
            pass
        else:
            raise AssertionError("action-set mismatch was accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest="command", required=True)
    validate = subcommands.add_parser("validate", help="validate retained producer and consumer directories")
    validate.add_argument("producer", type=Path)
    validate.add_argument("consumer", type=Path)
    validate.add_argument("output", type=Path)
    subcommands.add_parser("self-test", help="exercise the validator with synthetic retained evidence")
    arguments = parser.parse_args()
    if arguments.command == "self-test":
        self_test()
        print("native-cache remote pair validator self-test passed")
        return 0
    try:
        require(not arguments.output.exists(), f"output already exists: {arguments.output}")
        report = validate_pair(arguments.producer.resolve(), arguments.consumer.resolve())
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    except ValidationError as error:
        parser.exit(1, f"native-cache remote pair validation failed: {error}\n")
    print(f"native-cache remote pair validated: {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
