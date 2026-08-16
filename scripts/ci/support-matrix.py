#!/usr/bin/env python3
"""Validate support authorities and render their CI or documentation projection."""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import tomllib

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]


class ContractError(RuntimeError):
    """One checked support authority is malformed or disagrees with another."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def require_object(value: Any, path: str, keys: set[str]) -> dict[str, Any]:
    require(isinstance(value, dict), f"{path} must be an object")
    actual = set(value)
    require(
        actual == keys,
        f"{path} fields differ: expected {sorted(keys)}, found {sorted(actual)}",
    )
    return value


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(
            f"cannot load {path.relative_to(REPOSITORY_ROOT)}: {error}"
        ) from error


def load_toml(path: Path) -> dict[str, Any]:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ContractError(
            f"cannot load {path.relative_to(REPOSITORY_ROOT)}: {error}"
        ) from error


def require_string(value: Any, path: str) -> str:
    require(
        isinstance(value, str) and bool(value), f"{path} must be a non-empty string"
    )
    return value


def require_unique_sorted(values: list[str], path: str) -> None:
    require(len(values) == len(set(values)), f"{path} contains duplicates")
    require(values == sorted(values), f"{path} must be sorted")


@dataclass(frozen=True)
class NativeHost:
    target: str
    runner: str
    cache_key: str
    full_suite: bool
    filesystem: str
    case_sensitive: bool


@dataclass(frozen=True)
class FilesystemProfile:
    name: str
    target: str
    runner: str
    setup: str
    filesystem: str
    case_sensitive: bool


@dataclass(frozen=True)
class CrossTargetFixture:
    target: str
    fixture: str
    artifact: str


@dataclass(frozen=True)
class DeferredHost:
    name: str
    target: str
    evidence_gate: str


@dataclass(frozen=True)
class Task9NativeGate:
    target: str
    evidence_gate: str


@dataclass(frozen=True)
class Task9RemoteGate:
    provider: str
    evidence_gates: tuple[str, ...]


@dataclass(frozen=True)
class CompatibilityManifest:
    schema_version: int
    corpus_fixture: str
    corpus_runner: str
    cross_target_fixtures: tuple[CrossTargetFixture, ...]
    native_hosts: tuple[NativeHost, ...]
    filesystem_profiles: tuple[FilesystemProfile, ...]
    release_cross_targets: tuple[str, ...]
    deferred_hosts: tuple[DeferredHost, ...]
    task9_native_gates: tuple[Task9NativeGate, ...]
    task9_remote_gates: tuple[Task9RemoteGate, ...]
    alternate_linkers: tuple[dict[str, Any], ...]
    alternate_codegen_backends: tuple[dict[str, Any], ...]


def load_compatibility_manifest() -> CompatibilityManifest:
    path = REPOSITORY_ROOT / "tests/compatibility/manifest.json"
    raw = require_object(
        load_json(path),
        "compatibility manifest",
        {
            "schema_version",
            "front_door_corpus",
            "cross_target_corpus",
            "native_hosts",
            "filesystem_profiles",
            "required_release_cross_targets",
            "deferred_native_hosts",
            "task9_open_evidence_gates",
            "advertised_non_default_linkers",
            "advertised_non_default_codegen_backends",
        },
    )
    require(
        raw["schema_version"] == 7, "compatibility manifest schema_version must be 7"
    )
    corpus = require_object(
        raw["front_door_corpus"], "front_door_corpus", {"fixture", "runner"}
    )
    corpus_fixture = require_string(corpus["fixture"], "front_door_corpus.fixture")
    corpus_runner = require_string(corpus["runner"], "front_door_corpus.runner")
    require(
        (REPOSITORY_ROOT / corpus_fixture).is_dir(),
        f"front-door fixture does not exist: {corpus_fixture}",
    )
    require(
        (REPOSITORY_ROOT / corpus_runner).is_file(),
        f"front-door runner does not exist: {corpus_runner}",
    )

    cross_target_fixtures: list[CrossTargetFixture] = []
    require(
        isinstance(raw["cross_target_corpus"], list) and raw["cross_target_corpus"],
        "cross_target_corpus must be a non-empty array",
    )
    for index, value in enumerate(raw["cross_target_corpus"]):
        entry = require_object(
            value,
            f"cross_target_corpus[{index}]",
            {"target", "fixture", "artifact"},
        )
        fixture = require_string(
            entry["fixture"], f"cross_target_corpus[{index}].fixture"
        )
        require(
            (REPOSITORY_ROOT / fixture / "Cargo.toml").is_file(),
            f"cross-target fixture does not contain Cargo.toml: {fixture}",
        )
        artifact = require_string(
            entry["artifact"], f"cross_target_corpus[{index}].artifact"
        )
        require(
            artifact in {"rlib", "wasm"},
            f"cross_target_corpus[{index}].artifact is not rlib or wasm",
        )
        cross_target_fixtures.append(
            CrossTargetFixture(
                target=require_string(
                    entry["target"], f"cross_target_corpus[{index}].target"
                ),
                fixture=fixture,
                artifact=artifact,
            )
        )
    require_unique_sorted(
        [fixture.target for fixture in cross_target_fixtures],
        "cross_target_corpus targets",
    )

    native_hosts: list[NativeHost] = []
    require(
        isinstance(raw["native_hosts"], list) and raw["native_hosts"],
        "native_hosts must be a non-empty array",
    )
    for index, value in enumerate(raw["native_hosts"]):
        host = require_object(
            value,
            f"native_hosts[{index}]",
            {
                "target",
                "runner",
                "cache_key",
                "full_suite",
                "filesystem",
                "case_sensitive",
            },
        )
        require(
            host["full_suite"] is True, f"native_hosts[{index}].full_suite must be true"
        )
        require(
            isinstance(host["case_sensitive"], bool),
            f"native_hosts[{index}].case_sensitive must be a boolean",
        )
        native_hosts.append(
            NativeHost(
                target=require_string(host["target"], f"native_hosts[{index}].target"),
                runner=require_string(host["runner"], f"native_hosts[{index}].runner"),
                cache_key=require_string(
                    host["cache_key"], f"native_hosts[{index}].cache_key"
                ),
                full_suite=True,
                filesystem=require_string(
                    host["filesystem"], f"native_hosts[{index}].filesystem"
                ),
                case_sensitive=host["case_sensitive"],
            )
        )
    require_unique_sorted(
        [host.target for host in native_hosts], "native_hosts targets"
    )
    require(
        len({host.runner for host in native_hosts}) == len(native_hosts),
        "native_hosts runners must be unique",
    )
    require(
        len({host.cache_key for host in native_hosts}) == len(native_hosts),
        "native_hosts cache keys must be unique",
    )
    filesystem_profiles: list[FilesystemProfile] = []
    require(
        isinstance(raw["filesystem_profiles"], list) and raw["filesystem_profiles"],
        "filesystem_profiles must be a non-empty array",
    )
    native_by_target = {host.target: host for host in native_hosts}
    for index, value in enumerate(raw["filesystem_profiles"]):
        profile = require_object(
            value,
            f"filesystem_profiles[{index}]",
            {"name", "target", "runner", "setup", "filesystem", "case_sensitive"},
        )
        target = require_string(
            profile["target"], f"filesystem_profiles[{index}].target"
        )
        runner = require_string(
            profile["runner"], f"filesystem_profiles[{index}].runner"
        )
        native = native_by_target.get(target)
        require(
            native is not None,
            f"filesystem_profiles[{index}] target is not an advertised native host",
        )
        require(
            native.runner == runner,
            f"filesystem_profiles[{index}] must use the native host's runner",
        )
        require(
            isinstance(profile["case_sensitive"], bool),
            f"filesystem_profiles[{index}].case_sensitive must be a boolean",
        )
        setup = require_string(profile["setup"], f"filesystem_profiles[{index}].setup")
        require(
            setup in {"linux-tmpfs", "windows-ntfs-vhd"},
            f"filesystem_profiles[{index}].setup is not implemented by compatibility CI",
        )
        filesystem_profiles.append(
            FilesystemProfile(
                name=require_string(
                    profile["name"], f"filesystem_profiles[{index}].name"
                ),
                target=target,
                runner=runner,
                setup=setup,
                filesystem=require_string(
                    profile["filesystem"], f"filesystem_profiles[{index}].filesystem"
                ),
                case_sensitive=profile["case_sensitive"],
            )
        )
    require_unique_sorted(
        [profile.name for profile in filesystem_profiles], "filesystem profile names"
    )
    require(
        len({profile.setup for profile in filesystem_profiles})
        == len(filesystem_profiles),
        "filesystem profile setup modes must be unique",
    )

    release_cross_targets: list[str] = []
    require(
        isinstance(raw["required_release_cross_targets"], list)
        and raw["required_release_cross_targets"],
        "required_release_cross_targets must be a non-empty array",
    )
    for index, value in enumerate(raw["required_release_cross_targets"]):
        target = require_object(
            value, f"required_release_cross_targets[{index}]", {"target"}
        )
        release_cross_targets.append(
            require_string(
                target["target"], f"required_release_cross_targets[{index}].target"
            )
        )
    require_unique_sorted(release_cross_targets, "required_release_cross_targets")

    deferred_hosts: list[DeferredHost] = []
    require(
        isinstance(raw["deferred_native_hosts"], list),
        "deferred_native_hosts must be an array",
    )
    for index, value in enumerate(raw["deferred_native_hosts"]):
        host = require_object(
            value,
            f"deferred_native_hosts[{index}]",
            {"name", "target", "evidence_gate"},
        )
        deferred_hosts.append(
            DeferredHost(
                name=require_string(
                    host["name"], f"deferred_native_hosts[{index}].name"
                ),
                target=require_string(
                    host["target"], f"deferred_native_hosts[{index}].target"
                ),
                evidence_gate=require_string(
                    host["evidence_gate"],
                    f"deferred_native_hosts[{index}].evidence_gate",
                ),
            )
        )
    require_unique_sorted(
        [host.target for host in deferred_hosts], "deferred_native_hosts targets"
    )
    require_unique_sorted(
        [host.evidence_gate for host in deferred_hosts],
        "deferred_native_hosts evidence gates",
    )
    for host in deferred_hosts:
        require(
            re.fullmatch(r"native_[a-z0-9]+_hardware_access_unavailable", host.evidence_gate)
            is not None,
            f"deferred host {host.target} has an invalid hardware-access gate",
        )

    task9 = require_object(
        raw["task9_open_evidence_gates"],
        "task9_open_evidence_gates",
        {"native_hosts", "remote_providers"},
    )
    task9_native_gates: list[Task9NativeGate] = []
    require(
        isinstance(task9["native_hosts"], list) and task9["native_hosts"],
        "task9_open_evidence_gates.native_hosts must be a non-empty array",
    )
    for index, value in enumerate(task9["native_hosts"]):
        gate = require_object(
            value,
            f"task9_open_evidence_gates.native_hosts[{index}]",
            {"target", "evidence_gate"},
        )
        task9_native_gates.append(
            Task9NativeGate(
                target=require_string(
                    gate["target"], f"task9_open_evidence_gates.native_hosts[{index}].target"
                ),
                evidence_gate=require_string(
                    gate["evidence_gate"],
                    f"task9_open_evidence_gates.native_hosts[{index}].evidence_gate",
                ),
            )
        )
    require_unique_sorted(
        [gate.target for gate in task9_native_gates],
        "task9_open_evidence_gates.native_hosts targets",
    )
    expected_task9_native_targets = (
        {host.target for host in native_hosts}
        | {host.target for host in deferred_hosts}
        | {"aarch64-apple-darwin", "x86_64-apple-darwin"}
    )
    require(
        {gate.target for gate in task9_native_gates} == expected_task9_native_targets,
        "Task 9 native evidence registry must cover macOS plus every advertised or deferred native host",
    )

    task9_remote_gates: list[Task9RemoteGate] = []
    require(
        isinstance(task9["remote_providers"], list) and task9["remote_providers"],
        "task9_open_evidence_gates.remote_providers must be a non-empty array",
    )
    for index, value in enumerate(task9["remote_providers"]):
        gate = require_object(
            value,
            f"task9_open_evidence_gates.remote_providers[{index}]",
            {"provider", "evidence_gates"},
        )
        evidence_gates = gate["evidence_gates"]
        require(
            isinstance(evidence_gates, list)
            and bool(evidence_gates)
            and all(isinstance(item, str) and item for item in evidence_gates),
            f"task9_open_evidence_gates.remote_providers[{index}].evidence_gates "
            "must be a non-empty string array",
        )
        require_unique_sorted(
            evidence_gates,
            f"task9_open_evidence_gates.remote_providers[{index}].evidence_gates",
        )
        task9_remote_gates.append(
            Task9RemoteGate(
                provider=require_string(
                    gate["provider"],
                    f"task9_open_evidence_gates.remote_providers[{index}].provider",
                ),
                evidence_gates=tuple(evidence_gates),
            )
        )
    require_unique_sorted(
        [gate.provider for gate in task9_remote_gates],
        "task9_open_evidence_gates.remote_providers providers",
    )
    require(
        {gate.provider for gate in task9_remote_gates}
        == {"aws-s3", "azure-blob", "cloudflare-r2"},
        "Task 9 remote evidence registry must cover AWS S3, Azure Blob, and Cloudflare R2",
    )
    task9_gate_ids = [gate.evidence_gate for gate in task9_native_gates] + [
        gate_id for gate in task9_remote_gates for gate_id in gate.evidence_gates
    ]
    require(
        len(task9_gate_ids) == len(set(task9_gate_ids)),
        "Task 9 evidence gate IDs must be unique",
    )
    for gate_id in task9_gate_ids:
        require(
            re.fullmatch(r"[a-z0-9]+(?:_[a-z0-9]+)*", gate_id) is not None,
            f"Task 9 evidence gate has an invalid ID: {gate_id}",
        )

    alternate_linkers = raw["advertised_non_default_linkers"]
    alternate_backends = raw["advertised_non_default_codegen_backends"]
    require(
        isinstance(alternate_linkers, list),
        "advertised_non_default_linkers must be an array",
    )
    require(
        isinstance(alternate_backends, list),
        "advertised_non_default_codegen_backends must be an array",
    )
    native_targets = {host.target for host in native_hosts}
    linker_ids: list[str] = []
    for index, value in enumerate(alternate_linkers):
        entry = require_object(
            value,
            f"advertised_non_default_linkers[{index}]",
            {"id", "targets", "fixture"},
        )
        linker_ids.append(
            require_string(entry["id"], f"advertised_non_default_linkers[{index}].id")
        )
        fixture = require_string(
            entry["fixture"], f"advertised_non_default_linkers[{index}].fixture"
        )
        require(
            (REPOSITORY_ROOT / fixture).exists(),
            f"advertised linker fixture does not exist: {fixture}",
        )
        targets = entry["targets"]
        require(
            isinstance(targets, list)
            and bool(targets)
            and all(isinstance(target, str) and target for target in targets),
            f"advertised_non_default_linkers[{index}].targets must be a non-empty string array",
        )
        require_unique_sorted(
            targets, f"advertised_non_default_linkers[{index}].targets"
        )
        require(
            set(targets) <= native_targets,
            f"advertised linker {entry['id']} names a non-native target",
        )
    require_unique_sorted(linker_ids, "advertised_non_default_linkers IDs")

    backend_ids: list[str] = []
    for index, value in enumerate(alternate_backends):
        entry = require_object(
            value,
            f"advertised_non_default_codegen_backends[{index}]",
            {"id", "targets", "toolchain", "fixture"},
        )
        backend_ids.append(
            require_string(
                entry["id"], f"advertised_non_default_codegen_backends[{index}].id"
            )
        )
        require_string(
            entry["toolchain"],
            f"advertised_non_default_codegen_backends[{index}].toolchain",
        )
        fixture = require_string(
            entry["fixture"],
            f"advertised_non_default_codegen_backends[{index}].fixture",
        )
        require(
            (REPOSITORY_ROOT / fixture).exists(),
            f"advertised codegen fixture does not exist: {fixture}",
        )
        targets = entry["targets"]
        require(
            isinstance(targets, list)
            and bool(targets)
            and all(isinstance(target, str) and target for target in targets),
            f"advertised_non_default_codegen_backends[{index}].targets must be a non-empty string array",
        )
        require_unique_sorted(
            targets, f"advertised_non_default_codegen_backends[{index}].targets"
        )
        require(
            set(targets) <= native_targets,
            f"advertised codegen backend {entry['id']} names a non-native target",
        )
    require_unique_sorted(backend_ids, "advertised_non_default_codegen_backends IDs")

    all_targets = (
        [host.target for host in native_hosts]
        + [fixture.target for fixture in cross_target_fixtures]
        + release_cross_targets
        + [host.target for host in deferred_hosts]
    )
    require(
        len(all_targets) == len(set(all_targets)),
        "native, cross, and deferred target sets must be disjoint",
    )

    return CompatibilityManifest(
        schema_version=raw["schema_version"],
        corpus_fixture=corpus_fixture,
        corpus_runner=corpus_runner,
        cross_target_fixtures=tuple(cross_target_fixtures),
        native_hosts=tuple(native_hosts),
        filesystem_profiles=tuple(filesystem_profiles),
        release_cross_targets=tuple(release_cross_targets),
        deferred_hosts=tuple(deferred_hosts),
        task9_native_gates=tuple(task9_native_gates),
        task9_remote_gates=tuple(task9_remote_gates),
        alternate_linkers=tuple(alternate_linkers),
        alternate_codegen_backends=tuple(alternate_backends),
    )


@dataclass(frozen=True)
class NativeCacheContract:
    schema_version: int
    cache_class: str
    execution_contract: str


def required_source_value(source: str, pattern: str, name: str) -> str:
    matches = re.findall(pattern, source, flags=re.MULTILINE)
    require(len(matches) == 1, f"native-cache source must define exactly one {name}")
    return matches[0]


def load_native_cache_contract() -> NativeCacheContract:
    source_path = REPOSITORY_ROOT / "src/compiler/native_cache.rs"
    source = source_path.read_text(encoding="utf-8")
    require(
        "native-cache-capabilities.json" not in source,
        "native-cache runtime must not depend on a static toolchain registry",
    )
    cache_class = required_source_value(
        source,
        r'^const GRADUATED_NATIVE_CACHE_CLASS: &str = "([^"]+)";$',
        "GRADUATED_NATIVE_CACHE_CLASS",
    )
    execution_contract = required_source_value(
        source,
        r'^pub\(crate\) const DIRECT_EXECUTION_CONTRACT: &str = "([^"]+)";$',
        "DIRECT_EXECUTION_CONTRACT",
    )
    schema_version = int(
        required_source_value(
            source,
            r"^const NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION: u32 = ([0-9]+);$",
            "NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION",
        )
    )
    require(
        "load_direct_invocation" in source
        and "incremental_work_product_observation_unavailable" in source,
        "transparent native cache must retain direct activation and the incremental bypass",
    )
    bypass_source = source + (REPOSITORY_ROOT / "src/compiler/collector.rs").read_text(
        encoding="utf-8"
    ) + (REPOSITORY_ROOT / "src/compiler/invocation.rs").read_text(encoding="utf-8")
    for reason in (
        "alternate_compiler_program_identity_unavailable",
        "clippy_diagnostic_result_authority_unavailable",
        "coff_linker_evidence_unavailable",
        "compiler_diagnostic_replay_unavailable",
        "compiler_output_root_authority_unavailable",
        "cross_target_toolchain_evidence_unavailable",
        "custom_target_directory_authority_unavailable",
        "dynamic_dependency_execution_observation_unavailable",
        "doctest_execution_result_authority_unavailable",
        "explicit_link_argument_evidence_unavailable",
        "explicit_linker_evidence_unavailable",
        "incremental_work_product_observation_unavailable",
        "local_cache_unavailable",
        "moved_root_compiler_work_product_validation_unavailable",
        "platform_linker_evidence_unavailable",
        "remapped_path_observation_unavailable",
        "rustdoc_output_tree_observation_unavailable",
    ):
        require(
            f'"{reason}"' in bypass_source,
            f"native-cache source is missing stable bypass reason {reason}",
        )

    return NativeCacheContract(
        schema_version=schema_version,
        cache_class=cache_class,
        execution_contract=execution_contract,
    )


def validate_inventories(manifest: CompatibilityManifest) -> None:
    native_targets = {host.target for host in manifest.native_hosts}
    release_cross_targets = set(manifest.release_cross_targets)
    required_release_targets = native_targets | release_cross_targets

    release_entries = load_json(REPOSITORY_ROOT / "distribution/release-targets.json")
    require(
        isinstance(release_entries, list) and release_entries,
        "release target registry must be a non-empty array",
    )
    release_targets: list[str] = []
    for index, value in enumerate(release_entries):
        entry = require_object(
            value, f"release-targets[{index}]", {"target", "os", "archive"}
        )
        target = require_string(entry["target"], f"release-targets[{index}].target")
        require_string(entry["os"], f"release-targets[{index}].os")
        require(
            entry["archive"] in {"tar", "zip"},
            f"release-targets[{index}].archive is invalid",
        )
        release_targets.append(target)
    require_unique_sorted(release_targets, "release target registry")
    require(
        set(release_targets) == required_release_targets,
        "release target registry must equal advertised native hosts plus required release cross targets",
    )

    toolchain = load_toml(REPOSITORY_ROOT / "rust-toolchain.toml")
    toolchain_config = toolchain.get("toolchain", {})
    toolchain_targets = toolchain_config.get("targets")
    msrv = workspace_msrv()
    repository_toolchain = require_string(
        toolchain_config.get("channel"), "rust-toolchain.toml toolchain.channel"
    )
    require(
        re.fullmatch(
            r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", repository_toolchain
        )
        is not None,
        "rust-toolchain.toml toolchain.channel must be an exact major.minor.patch Rust release",
    )
    require(
        repository_toolchain == msrv,
        f"repository toolchain {repository_toolchain} must equal workspace MSRV {msrv}",
    )
    require(
        isinstance(toolchain_targets, list),
        "rust-toolchain.toml toolchain.targets must be an array",
    )
    require(
        all(isinstance(target, str) and target for target in toolchain_targets),
        "rust-toolchain.toml toolchain.targets must contain only strings",
    )
    require_unique_sorted(toolchain_targets, "rust-toolchain.toml toolchain.targets")
    require(
        required_release_targets <= set(toolchain_targets),
        "rust-toolchain.toml is missing advertised native or required release cross targets",
    )
    setup_action = (REPOSITORY_ROOT / ".github/actions/setup/action.yaml").read_text(
        encoding="utf-8"
    )
    require(
        re.search(
            rf"^[ \t]+toolchain:[ \t]+{re.escape(repository_toolchain)}[ \t]*$",
            setup_action,
            re.MULTILINE,
        )
        is not None,
        f"repository setup action must install repository toolchain {repository_toolchain}",
    )

    rail_config = load_toml(REPOSITORY_ROOT / ".config/rail.toml")
    rail_targets = rail_config.get("targets")
    require(
        isinstance(rail_targets, list), ".config/rail.toml targets must be an array"
    )
    require(
        all(isinstance(target, str) and target for target in rail_targets),
        ".config/rail.toml targets must contain only strings",
    )
    require_unique_sorted(rail_targets, ".config/rail.toml targets")
    require(
        required_release_targets <= set(rail_targets),
        ".config/rail.toml is missing advertised native or required release cross targets",
    )

    corpus_runner = (REPOSITORY_ROOT / manifest.corpus_runner).read_text(
        encoding="utf-8"
    )
    require(
        manifest.corpus_fixture in corpus_runner,
        "front-door corpus runner does not use the manifest's fixture",
    )
    compatibility_workflow = (
        REPOSITORY_ROOT / ".github/workflows/compatibility.yaml"
    ).read_text(encoding="utf-8")
    for fragment in (
        "--compatibility-matrix",
        "--filesystem-matrix",
        "--selection-probes",
        "--cross-target-mutation-probes",
        manifest.corpus_runner,
        "scripts/ci/run-filesystem-compatibility.py",
        "just build-all",
        "just test-all",
    ):
        require(
            fragment in compatibility_workflow,
            f"compatibility workflow is missing {fragment}",
        )
    for caller in (".github/workflows/bootstrap.yaml", ".github/workflows/commit.yaml"):
        source = (REPOSITORY_ROOT / caller).read_text(encoding="utf-8")
        require(
            "uses: ./.github/workflows/compatibility.yaml" in source,
            f"{caller} does not call the compatibility workflow",
        )


def github_matrix(manifest: CompatibilityManifest) -> str:
    include = [
        {
            "target": {
                "name": host.target,
                "runner": host.runner,
                "cache-key": host.cache_key,
                "test": host.full_suite,
            }
        }
        for host in manifest.native_hosts
    ]
    return json.dumps({"include": include}, separators=(",", ":"))


def workspace_msrv() -> str:
    manifest = load_toml(REPOSITORY_ROOT / "Cargo.toml")
    value = manifest.get("workspace", {}).get("package", {}).get("rust-version")
    require(
        isinstance(value, str)
        and re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value)
        is not None,
        "Cargo.toml workspace.package.rust-version must be an exact major.minor.patch Rust release",
    )
    return value


def compatibility_matrix(manifest: CompatibilityManifest) -> str:
    msrv = workspace_msrv()
    include = [
        {
            "compatibility": {
                "name": f"{host.target} / Rust {msrv}",
                "target": host.target,
                "runner": host.runner,
                "cache-key": f"{host.cache_key}-rust-{msrv}",
                "toolchain": msrv,
                "targets": ",".join(
                    fixture.target for fixture in manifest.cross_target_fixtures
                ),
                "release": msrv,
                "full-suite": host.full_suite,
                "selection-probes": True,
                "cross-target-mutation-probes": True,
                "filesystem": host.filesystem,
                "case-sensitive": host.case_sensitive,
            }
        }
        for host in manifest.native_hosts
    ]
    return json.dumps({"include": include}, separators=(",", ":"))


def filesystem_matrix(manifest: CompatibilityManifest) -> str:
    msrv = workspace_msrv()
    include = [
        {
            "filesystem": {
                "name": profile.name,
                "target": profile.target,
                "runner": profile.runner,
                "setup": profile.setup,
                "kind": profile.filesystem,
                "case-sensitive": profile.case_sensitive,
                "toolchain": msrv,
                "release": msrv,
            }
        }
        for profile in manifest.filesystem_profiles
    ]
    return json.dumps({"include": include}, separators=(",", ":"))


def formatted_list(values: tuple[str, ...]) -> str:
    return ", ".join(f"`{value}`" for value in values)


def render_markdown(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheContract,
) -> str:
    native_hosts = {host.target: host for host in manifest.native_hosts}
    corpus_cross_targets = {
        fixture.target for fixture in manifest.cross_target_fixtures
    }
    targets = sorted(
        set(native_hosts) | set(manifest.release_cross_targets) | corpus_cross_targets
    )
    rows: list[str] = []
    for target in targets:
        host = native_hosts.get(target)
        if host is None:
            execution = "Not a native host"
            cross = "Required compatibility build"
            release = (
                "Cross-built artifact required"
                if target in manifest.release_cross_targets
                else "Fixture artifact required"
            )
            cache = "Bypass: `cross_target_toolchain_evidence_unavailable`"
        else:
            execution = f"Advertised; full-suite CI required (`{host.runner}`)"
            cross = "—"
            release = "Native artifact required"
            linked_class = ""
            if target.endswith("-unknown-linux-gnu"):
                linked_class = (
                    " and certified default-ELF-linker producers/final artifacts"
                )
            cache = (
                f"Active for structurally eligible `{native_cache.cache_class}` units"
                + linked_class
                + "; exact compiler identity is part of every key"
            )
        rows.append(
            f"| `{target}` | {execution} | {cross} | {release} | {cache} |"
        )

    deferred_rows = [
        (
            f"| {host.name} | `{host.target}` | Blocked: `{host.evidence_gate}` | "
            "Structurally active when the exact compiler identity is captured |"
        )
        for host in manifest.deferred_hosts
    ]

    task9_gate_rows = [
        f"| Native host | `{gate.target}` | `{gate.evidence_gate}` |"
        for gate in manifest.task9_native_gates
    ]
    task9_gate_rows.extend(
        f"| Remote provider | `{gate.provider}` | {formatted_list(gate.evidence_gates)} |"
        for gate in manifest.task9_remote_gates
    )

    filesystem_rows = [
        (
            f"| Default `{host.target}` | `{host.runner}` | `{host.filesystem}` | "
            f"{'Sensitive' if host.case_sensitive else 'Insensitive'} | Full endpoint suite and native probe |"
        )
        for host in manifest.native_hosts
    ]
    filesystem_rows.extend(
        (
            f"| {profile.name} | `{profile.runner}` | `{profile.filesystem}` | "
            f"{'Sensitive' if profile.case_sensitive else 'Insensitive'} | "
            "Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |"
        )
        for profile in manifest.filesystem_profiles
    )

    linkers = (
        formatted_list(
            tuple(
                require_string(entry["id"], "alternate linker id")
                for entry in manifest.alternate_linkers
            )
        )
        if manifest.alternate_linkers
        else "None"
    )
    backends = (
        formatted_list(
            tuple(
                require_string(entry["id"], "alternate codegen backend id")
                for entry in manifest.alternate_codegen_backends
            )
        )
        if manifest.alternate_codegen_backends
        else "None"
    )

    return f"""# Caching

> Auto-generated from executable CI/release registries and native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `{manifest.schema_version}`;
> native-cache compiler-identity schema: `{native_cache.schema_version}`.

Planning removes jobs that do not need to run. Caching removes compiler or analysis work only when Cargo-Rail can
prove that the stored result still matches every relevant input.

| Cache | Purpose | Work skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after complete input revalidation | Workspace diagnostic collection |
| Native compiler result | Restore one eligible rustc result through Cargo's wrapper boundary | That rustc invocation |

The layers have separate eligibility. A lookup or Cargo `fresh` flag never authorizes reuse. Cargo-Rail revalidates the
input identity, action/result binding, manifest, and exact stored bytes owned by the layer. Incomplete evidence runs
cold with a stable reason. **Fast when proven. Normal Cargo when not.**

## Transparent native compiler-result cache

One machine-local setup enables verified L1 reuse for ordinary Cargo, nextest, Just, IDE, and CI invocations that use
the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status
cargo rail doctor native-cache
```

Setup losslessly sets Cargo's global `build.rustc-wrapper`, installs a private minimal launcher plus compiler-cache
worker, initializes the bounded local CAS, and writes a versioned receipt under the effective Cargo home. The receipt
binds the content and stable file generation of both executables; the worker authenticates itself and its launcher
before acquiring installation context. `--check` previews the exact Cargo field and private paths and exits 1 when
changes are pending. Setup refuses a conflicting wrapper, an environment or workspace configuration that shadows the
global field, an ambiguous Cargo home, linked authority paths, or receipt drift. Repeating setup verifies or repairs
only the same owned authority.

Cargo freshness and incremental compilation remain authoritative L0. Cargo invokes the wrapper only for compiler work
that L0 did not eliminate; Cargo-Rail never restores a target directory or synthesizes fingerprints. Incremental,
Clippy, rustdoc, response-file, information, cross-target, custom-target-directory, native proc-macro consumer,
COFF-linked output, explicitly configured linker, and otherwise unmodeled shapes execute the selected compiler chain
before session or CAS acquisition. Each cold boundary reports its stable missing capability. Exact compiler-owned
metadata, rlib, and static-archive results use L1 only when Cargo-Rail can identify the toolchain, complete bounded
source and native-search namespaces, compiler environment, dependency artifacts, arguments, outputs, and physical
workspace root. Certified default Apple and Linux ELF linker paths additionally admit build-script executables,
proc-macro producers, ordinary binaries, tests, examples, benchmarks, `dylib`, and `cdylib` outputs. Build-script
execution itself remains Cargo-owned cold work.

Every compiler process first enters one pre-Clap boundary that captures Cargo's selected program and byte-exact argv.
Transparent execution preserves the working directory, inherited non-private environment, wrapper order, streams, and
process status; the analysis role adds only its owned lint or observation-output arguments. The boundary distinguishes
the cache wrapper, workspace fact driver, and rustdoc proxy before loading a compiler session. Ambiguous roles fail
before Clap or compiler execution. Information requests, response files, clippy, incremental compilation, unsupported
crate types, and invocations without modeled outputs execute the original chain before session or CAS acquisition.

`CARGO_RAIL_CACHE=off` is the process-local kill switch for an already-selected Cargo-Rail compiler wrapper. The
minimal launcher directly executes the original compiler chain without starting the cache worker, reading the
installation receipt or session state, or opening the CAS. Use this control for deliberate cold baselines.

Each authenticated installation session binds one physical workspace root, and each compiler action additionally
binds the physical source namespace for that unit. Cargo-Rail therefore reuses exact path-bearing compiler artifacts
only within that root; a moved or independent checkout compiles cold and records its own exact variant. Each compiler
unit also binds its exact rustc arguments and cfg, dependency artifact contents, every entry and regular-file byte
below the crate-root directory, and every compiler-visible environment name and value after removing only Cargo-Rail's
exact private controls. Rustc observation then proves that the successful invocation selected no input outside those
capabilities. The action identity is available before lookup; lookup work does not grow with retained source history.

One protocol owns every accepted result. The pre-execution action binds the compiler session, argv, source topology,
environment, dependencies, generated inputs, and native-search state. Linked Linux ELF actions use that value only as
a non-authoritative candidate selector until the cold linker witness closes found and missing lookup paths, driver
and linker bytes, platform tool inputs, exact dependency archives, and rustc-endogenous objects. The witnessed action
then binds one result descriptor and exact output/stream objects. L1 verifies the same action, witness, result
association, and objects at restore time. L2 transports the same pack and cannot redefine its authority.

Linux ELF certification is similarly bounded. Cargo-Rail resolves the installed default `cc` driver and its selected
linker, requires GNU-compatible dependency-file evidence, and records direct driver inputs, selected auxiliary tools,
and driver/linker search directories. The cold witness binds every reported link input, closes relevant search
namespaces with found and missing same-name candidates, and treats only rustc-generated objects under the exact linked
output directory as endogenous. A changed driver, linker, tool, input, or symlink; an appeared negative lookup;
missing dependency evidence; no certified rustc object; or a non-byte-stable result runs cold and receives no linked
cache authority.

The cache deliberately over-invalidates when an unused file in the bounded source directory changes. This is the
smallest sound contract for Rust's path discovery: positive dep-info alone does not record failed probes such as the
choice between `foo.rs` and `foo/mod.rs`. A source symlink, unsupported file kind, incomplete capture, mutation during
capture, or exceeded entry, byte, depth, or time bound runs the original compiler instead of weakening that proof.

The session does not bind the complete Cargo configuration or `Cargo.lock`.
Changes to `build.warnings`, jobs, build or target directories, network policy, registry settings, and unrelated
lockfile entries can therefore reuse a result when those exact unit inputs remain unchanged. Rust flags, features,
dependency contents, target, linker, sysroot, source topology, and compiler environment still change or reject reuse
at their owning boundary.

Filesystem reads include files used through `include!`, `include_str!`, and `include_bytes!`. Rust metadata can contain
source roots, so Cargo-Rail never injects compiler remapping into a cache-requested invocation. The CAS uses reversible
tokens only for verified dep-info and JSON output-path bindings, including their Windows separator and escaping form,
then restores the current output paths after verification. Source-root authority remains physical and root-bound;
output names and materialized bytes remain exact, and ambiguous or unmodeled cached paths fail closed.
Cargo-Rail never changes incremental policy. An active or requested incremental invocation stays on Cargo's ordinary
path. The doctor reports the exact compiler identity and installation health without running a build. An existing
`rustc-workspace-wrapper` remains in Cargo's selected chain; Cargo-Rail bypasses reuse for that composition. Setup
refuses an existing `rustc-wrapper`, and the compiler boundary rejects ambiguous or recursive wrapper chains.

`cache status` reports installation health, Cargo L0 ownership, a bounded best-effort usage ledger, and exact CAS
ownership. Acquisition-free early bypasses are reported separately and do not touch session or CAS state. The ledger
is operational evidence, not reuse authority:

- `hit` means current inputs and every stored object were reverified before exact output bytes were restored;
- `miss` means the exact action has no authoritative result; successful cold output may populate the local CAS; and
- `bypass` means the session or invocation is outside the graduated class and rustc executed normally.

The corrected local store resolves an exact action to zero or one authoritative result. A second semantic result for
the same action becomes a durable conflict; malformed or transaction-ambiguous state becomes quarantined. Neither
state restores cached output. Restore uses one durable marker for the exact destination set. Failure before the first
visible replacement falls back to the original compiler; failure after that boundary cleans every owned destination
and fails without running rustc over a partial commit.

Corrupt or incompatible native entries never authorize reuse. Missing installation state, an unavailable CAS, or
incomplete pre-commit evidence executes cold. A failure after the first visible restore replacement fails closed so
rustc never runs over a partial restored output set.

## Shared native cache (L2)

Repository `[cache]` configuration is rejected because a checkout must not select a network write destination. Persist
one machine-owned authority during the same setup used for L1, then run ordinary Cargo without wrapper commands or
Cargo-Rail environment variables:

```bash
cargo rail cache setup --check --remote \\
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \\
  --remote-mode read-write
cargo rail cache setup --remote \\
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \\
  --remote-mode read-write
cargo build --workspace --all-features --locked
```

`loadingalias/cargo-rail-action/cache@v6` accepts the same URL as `url`, runs setup once, and leaves later ordinary
Cargo commands in that GitHub Actions job on the installed cache path. Configure provider credentials before Cargo
runs and invoke the cache Action separately in each execution job because hosted jobs do not share a machine or Cargo
home.

Validate or canonicalize a URL without resolving credentials or contacting storage:

```bash
cargo rail cache normalize \\
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012'
```

The accepted authority families are:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

Official S3 requires the expected 12-digit owner. R2 derives its documented account endpoint and region `auto`; Azure
derives the public Blob endpoint from the storage-account name. User information, fragments, encoded separators, dot
segments, duplicate or unknown query keys, ambiguous ports, and credentials are rejected before identity.

```bash
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

`--remote-mode` accepts `read` or `read-write` and defaults to `read-write` for an explicit selection. Read mode
requires an existing compatible protocol marker and never writes. Read-write mode conditionally creates that marker
and one compressed, bounded entry per base action. The entry binds the selected environment names, exact action,
result identity, and verified result pack. A second distinct identity conditionally replaces it with a terminal
metadata-only conflict. Build clients use only object reads and conditional writes; they do not list or delete objects.
`--local-only` removes persisted L2 activation without removing L1. The environment variables remain transient
overrides for qualification and advanced automation.

L1 remains authoritative. A verified L1 hit makes no remote request. Only an L1 miss contacts a short-lived private
loopback coordinator. It shares one AWS SDK runtime, credential resolution, client, and connection pool across rustc
processes, holds no build-result memory cache, and exits after bounded inactivity. Coordinator startup or IPC failure
falls back to the direct transport; every remote failure still compiles cold. An empty-L1 hit needs one entry GET after
the coordinator's protocol check. Downloaded packs must match the base action, selected environment names, exact
action, declared length, canonical descriptor, result identity, every payload digest, current compiler invocation,
source capture, and reviewed environment-name policy before L1 admits them.

Official S3 URLs require a region and expected 12-digit bucket-owner account. Cargo-Rail pins both on every object
operation and rejects configured endpoint overrides for official AWS authority. Credentials remain outside URLs,
configuration, result packs, compiler arguments, diagnostics, and cache keys. A domain-separated digest isolates live
coordinators by credential authority without storing raw credential values. Cargo-Rail removes remote selection and
credential environment from every compiler child it launches. Prefer a machine/container role, OIDC, or a
preconfigured profile; keep exported credentials job-scoped and least-privilege when a provider requires them.

`CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT` may add a sorted comma-separated set of reviewed, non-secret compiler
environment names to the built-in `CARGO_PKG_NAME` and `OUT_DIR` policy. Only value digests enter action identity; raw
values are never uploaded. An unapproved name bypasses L2 for that compiler unit without disabling an existing valid
L1 result.

Status schema 10 reports provider, protocol, mode, approved-name count, and a redacted authority identity as
`direct_transport_selected`; it never prints the URL. Status and doctor remain network-free.

AWS S3 and R2 use the same bounded conditional-object protocol; Azure Blob Storage implements the same cache protocol
through its native conditional operations. Cleartext transport exists only for deterministic loopback protocol
fixtures and is not a supported remote provider. No remote superiority claim is made until retained real-backend
measurements satisfy the benchmark contract.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, emitted outputs, executable identity, and recorded environment
reads. This store contains diagnostic evidence, not restorable Cargo artifacts.

An explicitly launched analysis run creates one private fact capability bound to its exact source root and observation
directory. Rustc and rustdoc publish only while that capability validates. An absent capability bypasses fact
collection and preserves the original compiler; an incomplete, moved, or tampered capability fails the analysis run
instead of silently accepting partial evidence. Fact identity remains separate from native cache action/result
identity.

Check mode does not edit manifests, but analysis may update cache and report files under `target/cargo-rail/`.
`cargo rail unify --check -f json` exposes `evidence_cache` hits, misses, and reasons.

## Storage and cleanup

Setup records the exact local authority base and positive byte bound in its receipt. The default base is the effective
Cargo home and the default result bound is 10 GiB; `--local-dir` and `--max-size` select explicit alternatives. Runtime
environment variables do not override the receipt, so setup, status, compiler reuse, and cleanup share one authority.
The private CAS validates its opaque owner marker before every use and never follows linked authority paths.

```bash
cargo rail cache setup --check
cargo rail cache status
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache remove --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup resolves only the currently
installed receipt authority, validates its owner marker, waits for in-flight readers, and removes that cross-workspace
CAS. Removing active L1 leaves setup drift that the next cold compiler invocation bypasses safely; `cache setup`
repairs the same root. `cache remove` losslessly removes only the receipt-owned Cargo field, wrapper, session state, and
receipt; it preserves CAS data. It refuses changed or unowned state. A legacy `local-cas-v1` is reclaim-only and never
becomes v2 authority. Use `--scope all` only when both cache-cleanup effects are intended. `cargo rail clean --cache`
remains a combined compatibility alias. Do not remove individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support and cache reuse are independent. A cache bypass still executes Cargo normally. Native reuse is
structural and exact-toolchain-keyed.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache |
|---|---|---|---|---|
{chr(10).join(rows)}

Linux musl rows are release cross-builds, not native Linux host evidence.

### Filesystems

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
{chr(10).join(filesystem_rows)}

Alternate profiles use bounded temporary volumes and detach them after success or failure.

### Deferred native hosts

| Platform | Target | Execution status | Cache status |
|---|---|---|---|
{chr(10).join(deferred_rows)}

IBM Power, IBM Z, and RISC-V need native hardware before Cargo-Rail can claim tested execution. Each row retains the
exact hardware-access gate; none is excluded by a runtime platform allowlist.

### Open Task 9 evidence gates

| Scope | Target or provider | Required evidence gate |
|---|---|---|
{chr(10).join(task9_gate_rows)}

These rows are explicit completion blockers, not unsupported-target declarations. Remove a gate only after retaining
the exact native or real-provider corpus required by the Task 9 contract.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | {linkers} | The default Apple driver/linker chain and a Linux `cc`-selected ELF linker with GNU-compatible dependency-file evidence are certified. Windows COFF linking reports `coff_linker_evidence_unavailable`; explicit linker selection or arguments report their own evidence boundary and execute unchanged. |
| Codegen backend | {backends} | Bundled named backends are bound by the complete sysroot identity and compiler arguments. An external backend path bypasses until its executable bytes are content-identified. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One physical-root-bound session and unit source namespace, complete bounded source topology and bytes, exact compiler environment, containment observation, `.rmeta`, optional `.rlib`, dependency contents, and exact native-static search namespaces |
| Incremental compilation | Bypassed before session or CAS acquisition | Cargo owns freshness and incremental policy; no stable compiler-owned validation interface exists for transported work products (`moved_root_compiler_work_product_validation_unavailable`) |
| Certified linked producers and final binaries | Active for the default Apple chain and a dependency-file-capable default ELF linker on Linux | One typed platform witness binds found/missing lookup namespaces, driver/linker and tool bytes, symlink resolution, dependency archives, certified rustc-generated objects, and byte-stable linked output; COFF and unknown providers retain explicit cold reasons |
| Tests, examples, and benchmarks | Active through a certified Apple or Linux ELF linker | Test harnesses are typed executable results; exact linker evidence and the normal compiler action authority bind their output |
| `dylib` and `cdylib` | Active through certified Apple or Linux ELF linking | COFF, custom, cross, signing, and unobserved post-link boundaries remain cold |
| `staticlib` | Active as a compiler-owned archive result | Rustc's archive builder owns the operation; the action binds source, upstream Rust/native archives, toolchain/backend, arguments, environment, and target, then validates the exact dep-info and archive bytes |
| Proc-macro producers | Metadata-only producer `.rmeta` is active directly; the producer dylib is active through certified Apple or Linux ELF linking | Neither result certifies later macro execution |
| Native proc-macro consumers | Bypassed before context acquisition | Compile-time filesystem, environment, process, network, clock, and randomness reads are not completely observed; unsafe sccache hits are not copied |
| Build-script executable compilation | Active through certified Apple or Linux ELF linking | The compiler result is reusable; build-script execution and its generated outputs remain Cargo-owned cold work |
| Native dependencies and `links` contracts | Exact native-static consumers may reuse metadata/rlib; native tools execute cold as typed child operations | The coverage graph identifies compiler probes, native compilation, assembly/preprocessing, archive steps, outputs, and downstream Rust consumers; incomplete dependency files, inherited environment, and archive mutation transactions retain specific cold reasons |
| Clippy diagnostics | Bypassed; Clippy executes | The compiler-mode ledger requires `clippy_diagnostic_result_authority_unavailable` before cache or remote acquisition |
| rustdoc and doctests | Bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree, and doctest execution is a separate result authority; the compiler-mode ledger proves both cold boundaries |
| Cross compilation and custom targets | Bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing workspace wrapper | Preserved; Cargo-Rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached; ambiguous and recursive chains are rejected |
| Existing global or environment wrapper | Setup refused or shadowed | Setup never silently replaces another `rustc-wrapper` authority |
| Custom Cargo target directory | Bypassed; Cargo executes | The wrapper cannot prove one physical workspace root from the standard `<root>/target` output layout |
| Per-invocation Cargo CLI `--config` | May shadow setup for that invocation | Cargo precedence remains authoritative; status and setup detect persistent environment and workspace shadowing |

## Benchmark evidence

The fixture contains registry and Git dependencies, build scripts, a proc macro, native code, a compiler-owned static
archive, Rust and C dynamic libraries, workspace libraries, binaries, tests, an example, and a benchmark target:

```bash
just bench-native-cache-smoke
just bench-native-cache 20
```

The current workflow invokes check, release-build, and test-target compilation directly under one isolated setup
receipt. It measures intact-target L0 overhead, empty-target L1 restoration, and cold-path overhead independently. The
comparison rotates lane order, preserves raw samples, records tool/host/configuration identity and usage counters, and
checks exact outputs before accepting a sample. The final report requires every compiler class represented by the
check/build/test corpus, so a fast subset cannot hide cold crate types. A separate compiler-mode ledger proves
acquisition-free Clippy, rustdoc, and doctest execution.

The canonical performance corpus contains 20 accepted interleaved samples per lane. Shared-hit superiority qualifies
only when transparent empty-target L1 is at least 10% faster than the pinned local `sccache` lane at both p50 and p95;
the target is 15%. The broader performance result also requires Cargo L0 and cache-off p95 overhead within the declared
bound and clean correctness and coverage acceptance. A failed corpus is a valid result: retain it and do not claim the
failed property. One accepted group is only a workflow smoke test; it is not performance qualification.

When evaluating another workspace, record the repository commit, tool and host/target identities, wrappers, flags,
target-state policy, disabled/cold/warm/sccache timings, hit and byte counts, bypass reasons, and byte-identity findings.
Never combine different commits, toolchains, targets, cache protocols, policies, or machines into one timing
population.
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    output = parser.add_mutually_exclusive_group(required=True)
    output.add_argument("--github-matrix", action="store_true")
    output.add_argument("--compatibility-matrix", action="store_true")
    output.add_argument("--filesystem-matrix", action="store_true")
    output.add_argument("--markdown", action="store_true")
    args = parser.parse_args()

    try:
        manifest = load_compatibility_manifest()
        native_cache = load_native_cache_contract()
        validate_inventories(manifest)
        if args.github_matrix:
            print(github_matrix(manifest))
        elif args.compatibility_matrix:
            print(compatibility_matrix(manifest))
        elif args.filesystem_matrix:
            print(filesystem_matrix(manifest))
        else:
            print(render_markdown(manifest, native_cache), end="")
        return 0
    except (ContractError, OSError) as error:
        print(f"support-matrix: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
