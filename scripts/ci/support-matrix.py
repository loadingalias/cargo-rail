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


def read_optional_nextest_version() -> str:
    config = load_toml(REPOSITORY_ROOT / ".config/nextest.toml")
    nextest_version = config.get("nextest-version")
    if isinstance(nextest_version, str):
        return require_string(nextest_version, ".config/nextest.toml nextest-version")
    require(isinstance(nextest_version, dict), "nextest.toml nextest-version must be a string or table")
    require(
        set(nextest_version) == {"required", "recommended"},
        "nextest.toml nextest-version must contain required and recommended",
    )
    required = require_string(
        nextest_version.get("required"), "nextest.toml nextest-version.required"
    )
    recommended = require_string(
        nextest_version.get("recommended"), "nextest.toml nextest-version.recommended"
    )
    require(
        required == recommended,
        "nextest.toml nextest-version required and recommended must match",
    )
    return required


def load_ci_tool_archives() -> tuple[CiToolArchive, ...]:
    path = REPOSITORY_ROOT / ".config/ci-tool-archives.tsv"
    lines = path.read_text(encoding="utf-8").splitlines()
    entries: list[CiToolArchive] = []
    keys: list[str] = []
    for index, line in enumerate(lines, start=1):
        trimmed = line.strip()
        if not trimmed or trimmed.startswith("#"):
            continue
        values = line.split("\t")
        require(
            len(values) == 7,
            f"ci-tool-archives.tsv line {index} must contain 7 tab columns",
        )
        tool = require_string(values[0], f"ci-tool-archives.tsv:{index}:tool")
        version = require_string(values[1], f"ci-tool-archives.tsv:{index}:version")
        os_name = require_string(values[2], f"ci-tool-archives.tsv:{index}:os")
        arch = require_string(values[3], f"ci-tool-archives.tsv:{index}:arch")
        filename = require_string(values[4], f"ci-tool-archives.tsv:{index}:filename")
        url = require_string(values[5], f"ci-tool-archives.tsv:{index}:url")
        digest = require_string(values[6], f"ci-tool-archives.tsv:{index}:sha256")
        require(
            re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None,
            f"ci-tool-archives.tsv:{index}:version must be semver",
        )
        require(re.fullmatch(r"[0-9a-f]{64}", digest) is not None, f"ci-tool-archives.tsv:{index}:sha256 must be a hex digest")
        require(
            url.startswith("https://"),
            f"ci-tool-archives.tsv:{index}:url must be https",
        )
        key = f"{tool}\t{version}\t{os_name}\t{arch}"
        keys.append(key)
        entries.append(
            CiToolArchive(
                tool=tool,
                version=version,
                os=os_name,
                arch=arch,
                filename=filename,
                url=url,
                sha256=digest,
            )
        )
    require(entries, "ci-tool-archives.tsv must contain at least one archive")
    require_unique_sorted(keys, "ci-tool-archives.tsv keys")
    require(len(keys) == len(set(keys)), "ci-tool-archives.tsv contains duplicate rows")
    return tuple(entries)


def require_string(value: Any, path: str) -> str:
    require(
        isinstance(value, str) and bool(value), f"{path} must be a non-empty string"
    )
    return value


def require_unique_sorted(values: list[str], path: str) -> None:
    require(len(values) == len(set(values)), f"{path} contains duplicates")
    require(values == sorted(values), f"{path} must be sorted")


def require_action_tool_version(paths: list[Path], tool: str, version: str) -> None:
    pattern = re.compile(
        rf"^[ \t]+tool:[ \t]+['\"]?{re.escape(tool)}(?:@([^'\"\s]+))?['\"]?[ \t]*$",
        re.MULTILINE,
    )
    installs = [
        (path, match.group(1))
        for path in paths
        for match in pattern.finditer(path.read_text(encoding="utf-8"))
    ]
    require(installs, f"GitHub Actions must install {tool}")
    require(
        all(installed_version == version for _, installed_version in installs),
        f"GitHub Actions {tool} installs must pin {tool}@{version}",
    )


@dataclass(frozen=True)
class NativeHost:
    target: str
    qualification: str
    runner: str | None
    cache_key: str | None
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
class CompatibilityManifest:
    schema_version: int
    corpus_fixture: str
    corpus_runner: str
    repository_coupled_rust_version_files: tuple[str, ...]
    cross_target_fixtures: tuple[CrossTargetFixture, ...]
    native_hosts: tuple[NativeHost, ...]
    filesystem_profiles: tuple[FilesystemProfile, ...]
    release_cross_targets: tuple[str, ...]
    deferred_hosts: tuple[DeferredHost, ...]
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
            "repository_coupled_rust_version_files",
            "cross_target_corpus",
            "native_hosts",
            "filesystem_profiles",
            "required_release_cross_targets",
            "deferred_native_hosts",
            "advertised_non_default_linkers",
            "advertised_non_default_codegen_backends",
        },
    )
    require(
        raw["schema_version"] == 9, "compatibility manifest schema_version must be 9"
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

    repository_msrv = workspace_msrv()
    repository_coupled_rust_version_files: list[str] = []
    require(
        isinstance(raw["repository_coupled_rust_version_files"], list)
        and raw["repository_coupled_rust_version_files"],
        "repository_coupled_rust_version_files must be a non-empty array",
    )
    for index, value in enumerate(raw["repository_coupled_rust_version_files"]):
        path = require_string(
            value,
            f"repository_coupled_rust_version_files[{index}]",
        )
        repository_file = REPOSITORY_ROOT / path
        require(repository_file.is_file(), f"{path} must be a file")
        if repository_file.suffix == ".toml":
            fixture_manifest = load_toml(repository_file)
            if isinstance(fixture_manifest.get("package"), dict):
                rust_version = fixture_manifest["package"].get("rust-version")
            else:
                rust_version = None
            if rust_version is None:
                workspace_package = (
                    fixture_manifest.get("workspace", {})
                    .get("package", {})
                )
                rust_version = workspace_package.get("rust-version")
            require(
                isinstance(rust_version, str),
                f"{path} must declare rust-version",
            )
            require(
                rust_version == repository_msrv,
                f"{path} rust-version must equal workspace MSRV {repository_msrv}",
            )
        else:
            source = repository_file.read_text(encoding="utf-8")
            require(
                re.search(
                    r"rust-version",
                    source,
                )
                is not None,
                f"{path} must contain a rust-version declaration",
            )
            require(
                f"rust-version = \"{repository_msrv}\"" in source,
                f"{path} must declare rust-version {repository_msrv}",
            )
        repository_coupled_rust_version_files.append(path)
    require_unique_sorted(
        repository_coupled_rust_version_files,
        "repository_coupled_rust_version_files",
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
        require(
            isinstance(value, dict), f"native_hosts[{index}] must be an object"
        )
        qualification = require_string(
            value.get("qualification"), f"native_hosts[{index}].qualification"
        )
        require(
            qualification in {"ci", "local"},
            f"native_hosts[{index}].qualification must be ci or local",
        )
        expected_fields = {
            "target",
            "qualification",
            "full_suite",
            "filesystem",
            "case_sensitive",
        }
        if qualification == "ci":
            expected_fields.update({"runner", "cache_key"})
        host = require_object(
            value,
            f"native_hosts[{index}]",
            expected_fields,
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
                qualification=qualification,
                runner=(
                    require_string(host["runner"], f"native_hosts[{index}].runner")
                    if qualification == "ci"
                    else None
                ),
                cache_key=(
                    require_string(
                        host["cache_key"], f"native_hosts[{index}].cache_key"
                    )
                    if qualification == "ci"
                    else None
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
    ci_hosts = [host for host in native_hosts if host.qualification == "ci"]
    require(
        len({host.runner for host in ci_hosts}) == len(ci_hosts),
        "CI-native host runners must be unique",
    )
    require(
        len({host.cache_key for host in ci_hosts}) == len(ci_hosts),
        "CI-native host cache keys must be unique",
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
            native.qualification == "ci" and native.runner == runner,
            f"filesystem_profiles[{index}] must use a CI-native host's runner",
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
            re.fullmatch(
                r"native_[a-z0-9]+_hardware_access_unavailable", host.evidence_gate
            )
            is not None,
            f"deferred host {host.target} has an invalid hardware-access gate",
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
        repository_coupled_rust_version_files=tuple(repository_coupled_rust_version_files),
        cross_target_fixtures=tuple(cross_target_fixtures),
        native_hosts=tuple(native_hosts),
        filesystem_profiles=tuple(filesystem_profiles),
        release_cross_targets=tuple(release_cross_targets),
        deferred_hosts=tuple(deferred_hosts),
        alternate_linkers=tuple(alternate_linkers),
        alternate_codegen_backends=tuple(alternate_backends),
    )


@dataclass(frozen=True)
class NativeCacheContract:
    schema_version: int
    cache_class: str
    execution_contract: str


@dataclass(frozen=True)
class CiToolArchive:
    tool: str
    version: str
    os: str
    arch: str
    filename: str
    url: str
    sha256: str


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
    bypass_source = (
        source
        + (REPOSITORY_ROOT / "src/compiler/collector.rs").read_text(encoding="utf-8")
        + (REPOSITORY_ROOT / "src/compiler/invocation.rs").read_text(encoding="utf-8")
    )
    for reason in (
        "alternate_compiler_program_identity_unavailable",
        "clippy_diagnostic_result_authority_unavailable",
        "coff_linker_evidence_unavailable",
        "compiler_diagnostic_replay_unavailable",
        "compiler_output_root_authority_unavailable",
        "cross_target_toolchain_evidence_unavailable",
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
            value,
            f"release-targets[{index}]",
            {"target", "os", "archive", "surface", "commit_ci"},
        )
        target = require_string(entry["target"], f"release-targets[{index}].target")
        require_string(entry["os"], f"release-targets[{index}].os")
        require(
            entry["archive"] in {"tar", "zip"},
            f"release-targets[{index}].archive is invalid",
        )
        require(
            isinstance(entry["surface"], bool),
            f"release-targets[{index}].surface must be a boolean",
        )
        require(
            isinstance(entry["commit_ci"], bool),
            f"release-targets[{index}].commit_ci must be a boolean",
        )
        native_host = next(
            (host for host in manifest.native_hosts if host.target == target), None
        )
        require(
            entry["commit_ci"]
            == (native_host is None or native_host.qualification != "local"),
            f"release-targets[{index}].commit_ci must exclude exactly locally qualified native hosts",
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
    toolchain_installer_path = "scripts/ci/install-rust-toolchain.sh"
    toolchain_installer = (REPOSITORY_ROOT / toolchain_installer_path).read_text(
        encoding="utf-8"
    )
    require(
        "rust-toolchain.toml" in toolchain_installer,
        "CI Rust installer must derive its default toolchain from rust-toolchain.toml",
    )
    for fragment in (
        'rustup_home="$RUNNER_TEMP/cargo-rail-rustup"',
        "--component cargo",
        "RUSTUP_TOOLCHAIN=%s",
        "RUSTUP_AUTO_INSTALL=0",
        'rustup which --toolchain "$selected" "$program"',
        'rustup run "$selected" "$program" --version',
    ):
        require(
            fragment in toolchain_installer,
            f"CI Rust installer is missing {fragment}",
        )
    require(
        toolchain_installer_path in setup_action,
        "repository setup action must use the CI Rust installer",
    )
    require(
        setup_action.index(toolchain_installer_path)
        < setup_action.index("taiki-e/install-action@")
        < setup_action.index("./.github/actions/cache"),
        "repository setup action must install Rust before tools or caches",
    )
    action_sources = [
        setup_action,
        *(
            path.read_text(encoding="utf-8")
            for path in sorted((REPOSITORY_ROOT / ".github/workflows").glob("*.yaml"))
        ),
    ]
    require(
        all("dtolnay/rust-toolchain@" not in source for source in action_sources),
        "GitHub Actions must keep Rust installation in the repository script",
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

    ci_tool_archives = load_ci_tool_archives()
    ci_tool_map = {(entry.tool, entry.version, entry.os, entry.arch): entry for entry in ci_tool_archives}
    install_tools = (REPOSITORY_ROOT / "scripts/ci/install-tools.sh").read_text(
        encoding="utf-8"
    )
    config_nextest_version = read_optional_nextest_version()
    require(
        "CI_TOOL_ARCHIVES" in install_tools
        and "ci_tool_archive cargo-nextest" in install_tools
        and "ci_tool_archive just" in install_tools,
        "scripts/ci/install-tools.sh must install cargo-nextest and just from ci-tool-archives.tsv",
    )
    require(
        re.search(r"readonly\s+(?:CARGO_NEXTEST_VERSION|JUST_VERSION)=", install_tools) is None,
        "scripts/ci/install-tools.sh must not duplicate cargo-nextest or just versions",
    )
    require(
        "cargo-audit" not in install_tools,
        "scripts/ci/install-tools.sh must keep cargo-deny as the single dependency policy gate",
    )
    action_tool_paths = [
        REPOSITORY_ROOT / ".github/actions/setup/action.yaml",
        *sorted((REPOSITORY_ROOT / ".github/workflows").glob("*.yaml")),
    ]

    nextest_required_pairs = {
        (entry.os, entry.arch)
        for entry in ci_tool_archives
        if entry.tool == "cargo-nextest"
    }
    require(
        bool(nextest_required_pairs),
        "ci-tool-archives.tsv must define cargo-nextest pins",
    )
    nextest_versions = {
        entry.version
        for entry in ci_tool_archives
        if entry.tool == "cargo-nextest"
    }
    require(len(nextest_versions) == 1, "ci-tool-archives.tsv must select one cargo-nextest version")
    nextest_version = next(iter(nextest_versions))
    require(
        nextest_version == config_nextest_version,
        "ci-tool-archives.tsv cargo-nextest version must match .config/nextest.toml",
    )
    require_action_tool_version(action_tool_paths, "cargo-nextest", nextest_version)
    expected_nextest_targets = {
        ("unknown-linux-gnu", "x86_64"),
        ("unknown-linux-gnu", "aarch64"),
        ("pc-windows-msvc", "x86_64"),
        ("pc-windows-msvc", "aarch64"),
    }
    require(nextest_required_pairs == expected_nextest_targets, "ci-tool-archives.tsv cargo-nextest targets are incomplete")
    for os_name, arch in sorted(expected_nextest_targets):
        entry = ci_tool_map[("cargo-nextest", nextest_version, os_name, arch)]
        target = f"{arch}-{os_name}"
        expected_filename = f"cargo-nextest-{nextest_version}-{target}.tar.gz"
        expected_url = (
            f"https://github.com/nextest-rs/nextest/releases/download/"
            f"cargo-nextest-{nextest_version}/{expected_filename}"
        )
        require(entry.filename == expected_filename and entry.url == expected_url, f"invalid cargo-nextest archive row for {target}")

    just_required_pairs = {
        (entry.os, entry.arch) for entry in ci_tool_archives if entry.tool == "just"
    }
    require(
        bool(just_required_pairs),
        "ci-tool-archives.tsv must define just pins",
    )
    just_versions = {entry.version for entry in ci_tool_archives if entry.tool == "just"}
    require(len(just_versions) == 1, "ci-tool-archives.tsv must select one just version")
    just_version = next(iter(just_versions))
    require_action_tool_version(action_tool_paths, "just", just_version)
    cargo_deny_match = re.search(
        r"^readonly CARGO_DENY_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$",
        install_tools,
        re.MULTILINE,
    )
    require(
        cargo_deny_match is not None,
        "scripts/ci/install-tools.sh must define one exact CARGO_DENY_VERSION",
    )
    require_action_tool_version(
        action_tool_paths,
        "cargo-deny",
        cargo_deny_match.group(1),
    )
    cargo_rail_match = re.search(
        r"^readonly CARGO_RAIL_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$",
        install_tools,
        re.MULTILINE,
    )
    require(
        cargo_rail_match is not None,
        "scripts/ci/install-tools.sh must install one exact Cargo-Rail release",
    )
    cache_action = (REPOSITORY_ROOT / ".github/actions/cache/action.yaml").read_text(
        encoding="utf-8"
    )
    for fragment in (
        "loadingalias/cargo-rail-action/cache@",
        "# v8",
        f"version: {cargo_rail_match.group(1)}",
        "scripts/cache/setup.sh --max-size 10GiB",
        "remote-credentials-ready:",
        "r2://*)",
    ):
        require(
            fragment in cache_action,
            f"repository cache action is missing {fragment}",
        )
    require(
        "configure-aws-credentials" not in cache_action
        and "AWS_ACCESS_KEY_ID" not in cache_action
        and "AWS_SECRET_ACCESS_KEY" not in cache_action,
        "repository cache action must not receive or export provider credentials",
    )
    require(
        "--root-portability remap"
        in (REPOSITORY_ROOT / "scripts/cache/setup.sh").read_text(encoding="utf-8"),
        "repository cache setup must qualify cross-checkout identities",
    )
    expected_just_targets = {
        ("unknown-linux-musl", "x86_64"),
        ("unknown-linux-musl", "aarch64"),
        ("pc-windows-msvc", "x86_64"),
        ("pc-windows-msvc", "aarch64"),
    }
    require(just_required_pairs == expected_just_targets, "ci-tool-archives.tsv just targets are incomplete")
    for os_name, arch in sorted(expected_just_targets):
        entry = ci_tool_map[("just", just_version, os_name, arch)]
        target = f"{arch}-{os_name}"
        suffix = "zip" if os_name == "pc-windows-msvc" else "tar.gz"
        expected_filename = f"just-{just_version}-{target}.{suffix}"
        expected_url = f"https://github.com/casey/just/releases/download/{just_version}/{expected_filename}"
        require(entry.filename == expected_filename and entry.url == expected_url, f"invalid just archive row for {target}")

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
        "python3 scripts/ci/support-matrix.py --github-matrix >/dev/null",
        toolchain_installer_path,
        "--selection-probes",
        "--cross-target-mutation-probes",
        manifest.corpus_runner,
        "scripts/ci/run-filesystem-compatibility.py",
        "if: fromJSON(needs.support.outputs.compatibility-matrix).include[0] != null",
        "if: fromJSON(needs.support.outputs.filesystem-matrix).include[0] != null",
        "just build-all",
        "just test-all",
        "cargo nextest run --workspace -P commit --all-features --locked --config-file .config/nextest.toml",
        "cargo test --doc -p cargo-rail --all-features --locked",
    ):
        require(
            fragment in compatibility_workflow,
            f"compatibility workflow is missing {fragment}",
        )
    require(
        compatibility_workflow.count(toolchain_installer_path) == 2,
        "compatibility workflow must install Rust in both execution jobs",
    )
    exact_driver_step = """\
      - name: Qualify exact compiler fact driver
        if: matrix.compatibility.full-suite
        shell: bash
        env:
          AWS_ACCESS_KEY_ID: ${{ secrets.r2_access_key_id }}
          AWS_SECRET_ACCESS_KEY: ${{ secrets.r2_secret_access_key }}
          AWS_EC2_METADATA_DISABLED: "true"
        run: |
          rustup component add rustc-dev --toolchain "$RUSTUP_TOOLCHAIN"
          just check-compiler-fact-driver
"""
    require(
        exact_driver_step in compatibility_workflow,
        "compatibility workflow must install rustc-dev and qualify the exact compiler fact driver under Bash",
    )
    for caller in (".github/workflows/bootstrap.yaml", ".github/workflows/commit.yaml"):
        source = (REPOSITORY_ROOT / caller).read_text(encoding="utf-8")
        require(
            "uses: ./.github/workflows/compatibility.yaml" in source,
            f"{caller} does not call the compatibility workflow",
        )

    commit_workflow = (
        REPOSITORY_ROOT / ".github/workflows/commit.yaml"
    ).read_text(encoding="utf-8")
    for fragment in (
        "vars.CARGO_RAIL_CACHE_REMOTE",
        "secrets.CARGO_RAIL_R2_ACCESS_KEY_ID",
        "secrets.CARGO_RAIL_R2_SECRET_ACCESS_KEY",
        "native-cache-credentials-ready:",
        "r2_access_key_id:",
        "r2_secret_access_key:",
    ):
        require(
            fragment in commit_workflow,
            f"Commit workflow is missing R2 cache authority: {fragment}",
        )
    for legacy in (
        "CACHE_QUALIFICATION_AWS_",
        "configure-aws-credentials",
        "native-cache-role:",
        "native-cache-region:",
        "native-cache-account:",
    ):
        require(
            legacy not in commit_workflow,
            f"Commit workflow retains legacy AWS cache wiring: {legacy}",
        )

    release_workflow = (REPOSITORY_ROOT / ".github/workflows/release.yaml").read_text(
        encoding="utf-8"
    )
    for fragment in (
        "gh run list",
        "actions/runs/$run_id/artifacts",
        "selected-matrix: ${{ needs.verify-release.outputs.matrix }}",
        "run-id: ${{ needs.verify-release.outputs.commit_run_id }}",
        "native-cache-url: ${{ vars.CARGO_RAIL_CACHE_REMOTE }}",
        "r2_access_key_id: ${{ secrets.CARGO_RAIL_R2_ACCESS_KEY_ID }}",
        "r2_secret_access_key: ${{ secrets.CARGO_RAIL_R2_SECRET_ACCESS_KEY }}",
    ):
        require(
            fragment in release_workflow,
            f"release workflow is missing exact-SHA archive reuse: {fragment}",
        )
    for duplicated_gate in ("cargo " + "nextest run", "cargo " + "deny", "Verify Clippy"):
        require(
            duplicated_gate not in release_workflow,
            f"release workflow repeats the Commit gate: {duplicated_gate}",
        )

    worker_verifier = "scripts/ci/verify-distributed-worker.py"
    require(
        (REPOSITORY_ROOT / worker_verifier).is_file(),
        "distributed worker shipment verifier is missing",
    )
    require(
        worker_verifier in compatibility_workflow,
        "compatibility workflow does not verify the distributed worker front door",
    )
    archive_workflow = (
        REPOSITORY_ROOT / ".github/workflows/release-archives.yaml"
    ).read_text(encoding="utf-8")
    for fragment in (
        "workflow_call:",
        toolchain_installer_path,
        "distribution/release-targets.json",
        "scripts/ci/smoke-release-tar.sh",
        "if: inputs.stage",
        "actions/attest@",
        "actions/upload-artifact@",
        "secrets.r2_access_key_id",
        "secrets.r2_secret_access_key",
        "remote-credentials-ready:",
    ):
        require(
            fragment in archive_workflow,
            f"release archive workflow is missing {fragment}",
        )
    archive_smoke = (
        REPOSITORY_ROOT / "scripts/ci/smoke-release-tar.sh"
    ).read_text(encoding="utf-8")
    for fragment in (
        worker_verifier,
        'smoke="$(mktemp -d',
        "--cargo-rail-fact-protocol-version",
        "capture_surface stable-prepare",
        "capture_surface stable-check",
        "capture_surface nightly-prepare",
        "capture_surface nightly-check",
    ):
        require(
            fragment in archive_smoke,
            f"release archive smoke command is missing {fragment}",
        )
    require(
        "rustup component list" not in archive_workflow,
        "release archive workflow must prove Surface capability instead of Rustup component inventory",
    )
    for workflow_path in sorted((REPOSITORY_ROOT / ".github/workflows").glob("*.yaml")):
        for line in workflow_path.read_text(encoding="utf-8").splitlines():
            if "rustup component add rustc-dev" in line:
                require(
                    '--toolchain "$RUSTUP_TOOLCHAIN"' in line,
                    f"{workflow_path.relative_to(REPOSITORY_ROOT)} must add rustc-dev to the selected toolchain",
                )
    for caller, stage in (
        (
            ".github/workflows/commit.yaml",
            "stage: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
        ),
        (".github/workflows/release.yaml", "stage: true"),
    ):
        source = (REPOSITORY_ROOT / caller).read_text(encoding="utf-8")
        require(
            "uses: ./.github/workflows/release-archives.yaml" in source
            and stage in source,
            f"{caller} does not call the release archive workflow with {stage}",
        )

    deny_graph = load_toml(REPOSITORY_ROOT / "deny.toml").get("graph")
    deny_graph = require_object(
        deny_graph,
        "deny.toml [graph]",
        {"targets"},
    )
    deny_targets = deny_graph["targets"]
    require(
        isinstance(deny_targets, list)
        and deny_targets
        and all(isinstance(target, str) and target for target in deny_targets),
        "deny.toml [graph].targets must be a non-empty string array",
    )
    require_unique_sorted(deny_targets, "deny.toml [graph].targets")

    expected_deny_targets = sorted(
        {host.target for host in manifest.native_hosts}
        | {target for target in manifest.release_cross_targets}
    )
    require(
        set(deny_targets) == set(expected_deny_targets),
        "deny.toml graph targets must match supported native hosts and release cross targets",
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
        if host.qualification == "ci"
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
        if host.qualification == "ci"
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


def render_markdown(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheContract,
) -> str:
    native_hosts = {host.target: host for host in manifest.native_hosts}
    cross_targets = {fixture.target for fixture in manifest.cross_target_fixtures}
    targets = sorted(
        set(native_hosts) | set(manifest.release_cross_targets) | cross_targets
    )
    target_rows: list[str] = []
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
            execution = (
                f"Advertised; full-suite CI required (`{host.runner}`)"
                if host.qualification == "ci"
                else "Advertised; local full-suite qualification required"
            )
            cross = "—"
            release = "Native artifact required"
            linked = (
                "; certified default-ELF linked outputs are also active"
                if target.endswith("-unknown-linux-gnu")
                else ""
            )
            cache = (
                f"Active for eligible `{native_cache.cache_class}` units"
                f"{linked}; exact compiler identity is part of every key"
            )
        target_rows.append(
            f"| `{target}` | {execution} | {cross} | {release} | {cache} |"
        )

    filesystem_rows: list[str] = []
    for host in manifest.native_hosts:
        runner = (
            f"`{host.runner}`"
            if host.qualification == "ci"
            else "Local native host"
        )
        evidence = (
            "Full endpoint suite and native probe"
            if host.qualification == "ci"
            else "Local full endpoint suite, native probe, and benchmarks"
        )
        filesystem_rows.append(
            f"| Default `{host.target}` | {runner} | `{host.filesystem}` | "
            f"{'Sensitive' if host.case_sensitive else 'Insensitive'} | {evidence} |"
        )
    for profile in manifest.filesystem_profiles:
        evidence = (
            "Front-door corpus, CAS/atomicity suite, cross-volume staging, ENOSPC, and cleanup"
            if profile.name == "windows-ntfs-vhd"
            else "Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup"
        )
        filesystem_rows.append(
            f"| {profile.name} | `{profile.runner}` | `{profile.filesystem}` | "
            f"{'Sensitive' if profile.case_sensitive else 'Insensitive'} | {evidence} |"
        )

    deferred_rows = [
        (
            f"| {host.name} | `{host.target}` | Blocked: `{host.evidence_gate}` | "
            "Structurally active when the exact compiler identity is captured |"
        )
        for host in manifest.deferred_hosts
    ]

    return f"""# Caching

> Auto-generated from executable support registries and native-cache gates. Do not edit manually.
>
> Regenerate with `just gen-docs`. Support manifest schema: `{manifest.schema_version}`;
> native-cache compiler-identity schema: `{native_cache.schema_version}`.

Cargo-Rail preserves Cargo as the executor. Each layer may remove only the work it can prove reusable.

| Layer | Authority | Result |
|---|---|---|
| Cargo L0 | Cargo fingerprints and incremental state | Cargo skips fresh work |
| Local L1 | Exact compiler action, result, and stored bytes | One compiler result is restored |
| Remote L2 | The same verified result pack under machine-owned provider authority | A result enters L1, then restores |
| Distributed miss | A pinned worker capability and validated response | One eligible miss executes remotely |
| Compiler evidence | Revalidated diagnostic observations | `unify` skips diagnostic collection |

A lookup is not authority. Missing, stale, ambiguous, unsupported, or corrupt evidence executes the normal compiler
path with a stable miss or bypass reason.

## Set up local reuse

One setup enables L1 for ordinary Cargo, nextest, Just, IDE, and CI commands using the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local
cargo rail doctor native-cache
```

Setup previews and then owns one global `build.rustc-wrapper`, private launcher and worker bytes, a receipt, and a
bounded CAS. It refuses another global wrapper, persistent shadowing, ambiguous Cargo homes, linked authority paths,
or changed receipt-owned state. Repeating setup verifies or repairs only the same authority.

Cargo freshness and incremental compilation remain L0. An L1 action binds the compiler, toolchain, arguments, target,
environment, dependencies, source topology and bytes, native-search inputs, physical workspace root, and declared
outputs. Stored descriptors and every output byte are reverified before restore. The bounded source capture
deliberately over-invalidates when it cannot prove that an unused path was irrelevant.

A result is:

- a `hit` only after current inputs and stored bytes verify;
- a `miss` when no authoritative result exists and successful cold output may populate the CAS;
- a `bypass` when the invocation is outside the supported class and executes normally; or
- a `conflict` when one action produced distinct semantic results, in which case neither result restores.

Disable reuse for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

## Share results remotely

Remote selection is machine state, never repository configuration. Persist L2 during setup, then use ordinary Cargo:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
```

Accepted URL families are:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

### Cloudflare R2

Use a private, default-jurisdiction bucket and an R2 API token scoped to Object Read & Write for that bucket. The
token's S3 access key ID and secret access key must be present for setup and every compiler process that should use
L2; a Wrangler login or Cloudflare API token is not an S3 credential pair.

```bash
export AWS_ACCESS_KEY_ID='<R2 access key ID>'
export AWS_SECRET_ACCESS_KEY='<R2 secret access key>'

cargo rail cache normalize \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'
cargo rail cache setup --check --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
cargo rail cache setup --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
```

Use distinct bucket-scoped credential pairs for CI and developer machines even when they share one R2 authority.
Keep the protocol marker at `native-v5/protocol`; a lifecycle rule may expire `native-v5/entries/` without deleting
the marker. Scope the prefix relative to the selected URL root when the URL includes a prefix.

Use `cargo rail cache normalize URL` to validate a URL without resolving credentials or contacting storage.
Credentials stay outside URLs, repository configuration, result packs, diagnostics, compiler arguments, and cache
keys. Prefer a machine or container role, OIDC, or a preconfigured profile.

`--remote-mode read` requires an existing compatible protocol marker and never writes. `read-write` adds conditional
protocol and entry publication. For an authority rooted at `PREFIX`, provider permissions are bounded to:

| Mode | Objects | Operations |
|---|---|---|
| `read` | `PREFIX/native-v5/protocol`, `PREFIX/native-v5/entries/*` | Object read |
| `read-write` | The same objects | Object read and conditional write |

Build credentials do not need list, delete, lifecycle, multipart-upload, or administrative authority. Keep provider
cleanup and lifecycle policy outside build credentials.

L1 remains authoritative, so an L1 hit makes no remote request. Absence, conflict, corruption, credential failure,
throttling, or outage executes the compiler. `--local-only` removes persisted L2 selection while preserving L1.

Physical-root mode is the default. It shares only checkouts at the same canonical path. Cross-root reuse requires
`--root-portability remap`; that mode admits only certified workspace Rust metadata and library results. Existing
remaps, external or generated source namespaces, native-search inputs, ambiguous roots, and unsupported output classes
bypass cross-root reuse.

Additional L2 environment names must be reviewed and non-secret. Select them with repeated `--remote-environment`
options during setup. Only value digests enter identity; raw values are not uploaded.

## Distribute eligible misses

Distributed execution runs below Cargo L0, L1, and L2. It accepts only bounded compiler-only Rust operations with
complete source and dependency inputs. Linked outputs, build scripts, generated namespaces, native dependencies,
unmodeled options, and newly observed compiler environments remain local.

The client requires one complete mTLS worker authority:

```bash
cargo rail cache setup --check \
  --distributed-endpoint '10.0.0.20:39443' \
  --distributed-server-name worker.example.internal \
  --distributed-capability 'worker-capability-v3:sha256:CAPABILITY_DIGEST' \
  --distributed-authority /etc/cargo-rail/server-ca.pem \
  --distributed-client-certificate /etc/cargo-rail/client.pem \
  --distributed-client-private-key /etc/cargo-rail/client.key
```

Run setup without `--check` only after reviewing the authority. The default `automatic` policy stays local until
fresh class-specific measurements predict a critical-path win. `qualification` sends every eligible miss to collect
evidence and may be slower.

Deploy the direct worker only on a dedicated single-tenant host or ephemeral VM. The qualified Linux mode pins the
compiler and worker capability, uses mutual TLS, runs each attempt inside a resource-bounded cgroup and Bubblewrap
sandbox, and inherits no operator or provider credentials. Use the repository's
`qualify-distributed-execution-*` recipes before serving a worker.

A transport, worker, lease, sandbox, or pre-commit validation failure executes the normalized operation locally once.
A successful response still passes the native-cache validation and restore transaction before Cargo sees output.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating their compiler, source, manifests,
targets, features, Cargo configuration, dependencies, outputs, executable identity, and observed environment reads.
This store contains diagnostic evidence, not restorable Cargo artifacts.

Check mode may update evidence under `target/cargo-rail/`. Inspect `evidence_cache` in JSON output for hits, misses,
and reasons.

## Inspect, clean, or remove

```bash
cargo rail cache status --scope local --format json
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache remove --check
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup removes only the
receipt-selected shared CAS after validating ownership and waiting for readers; rerun `cache setup` afterward.
`cache remove` removes the owned Cargo field and private setup state but preserves CAS data. Every operation refuses
changed, shadowed, linked, or unowned authority. Do not delete individual CAS objects or Cargo fingerprints by hand.

## Execution and reuse support

Execution support and cache reuse are independent. A bypass still executes Cargo normally.

### Hosts and targets

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache |
|---|---|---|---|---|
{chr(10).join(target_rows)}

Linux musl rows are release cross-builds, not native Linux host evidence.

### Filesystems

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
{chr(10).join(filesystem_rows)}

### Deferred native hosts

| Platform | Target | Execution status | Cache status |
|---|---|---|---|
{chr(10).join(deferred_rows)}

Deferred hosts need native hardware before Cargo-Rail can claim tested execution.

### Compiler classes

| Class | Reuse boundary |
|---|---|
| Metadata and Rust libraries | Active with exact toolchain, source, dependency, environment, and output identity |
| Certified Apple and Linux linked outputs | Active only with complete default-linker input evidence and byte-stable output |
| Tests, examples, benchmarks, `dylib`, and `cdylib` | Active only through a certified linker path |
| `staticlib` | Active as an exact compiler-owned archive result |
| Proc-macro producers | Metadata is active; linked producer output needs a certified linker; later macro execution is not covered |
| Native proc-macro consumers | Bypass because compile-time external reads are incomplete |
| Build-script compilation | Compiler result may reuse; script execution and generated output remain Cargo-owned cold work |
| Native dependencies and `links` | Rust consumers may reuse only with complete native-search evidence; native tools execute cold |
| Incremental, Clippy, rustdoc, and doctests | Bypass before result acquisition |
| Cross targets, custom targets or target directories, and unsupported wrappers | Execute through the selected compiler chain |

The default Apple linker chain and a Linux `cc`-selected ELF linker with GNU-compatible dependency evidence are
certified. Windows COFF linking, explicit linker selection, custom linker arguments, and external codegen backends
execute normally until their complete input boundary is graduated.

## Benchmark evidence

Use [the benchmarking contract](benchmarking.md) for smoke, qualification, correctness, evidence retention, and claim
requirements. Generated support rows describe tested authority; they are not performance claims.
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
