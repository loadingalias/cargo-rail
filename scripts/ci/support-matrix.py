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
class CustomTargetJson:
    toolchain: str
    target: str
    runner: str
    base_target: str
    fixture: str


@dataclass(frozen=True)
class DeferredHost:
    name: str
    target: str


@dataclass(frozen=True)
class CompatibilityManifest:
    schema_version: int
    corpus_fixture: str
    corpus_runner: str
    cross_target_fixtures: tuple[CrossTargetFixture, ...]
    custom_target_json: CustomTargetJson
    native_hosts: tuple[NativeHost, ...]
    filesystem_profiles: tuple[FilesystemProfile, ...]
    release_cross_targets: tuple[str, ...]
    deferred_hosts: tuple[DeferredHost, ...]
    alternate_linkers: tuple[dict[str, Any], ...]
    alternate_codegen_backends: tuple[dict[str, Any], ...]


@dataclass(frozen=True, order=True)
class RustRelease:
    major: int
    minor: int
    patch: int

    @classmethod
    def parse(cls, value: str, path: str) -> RustRelease:
        match = re.fullmatch(
            r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value
        )
        require(
            match is not None, f"{path} must be an exact major.minor.patch Rust release"
        )
        return cls(*(int(component) for component in match.groups()))

    def __str__(self) -> str:
        return f"{self.major}.{self.minor}.{self.patch}"


def load_compatibility_manifest() -> CompatibilityManifest:
    path = REPOSITORY_ROOT / "tests/compatibility/manifest.json"
    raw = require_object(
        load_json(path),
        "compatibility manifest",
        {
            "schema_version",
            "front_door_corpus",
            "cross_target_corpus",
            "custom_target_json",
            "native_hosts",
            "filesystem_profiles",
            "required_release_cross_targets",
            "deferred_native_hosts",
            "advertised_non_default_linkers",
            "advertised_non_default_codegen_backends",
        },
    )
    require(
        raw["schema_version"] == 4, "compatibility manifest schema_version must be 4"
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

    custom_raw = require_object(
        raw["custom_target_json"],
        "custom_target_json",
        {"toolchain", "target", "runner", "base_target", "fixture"},
    )
    custom_toolchain = require_string(
        custom_raw["toolchain"], "custom_target_json.toolchain"
    )
    require(
        re.fullmatch(r"nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}", custom_toolchain)
        is not None,
        "custom_target_json.toolchain must be a dated nightly",
    )
    custom_base_target = require_string(
        custom_raw["base_target"], "custom_target_json.base_target"
    )
    custom_fixture = require_string(custom_raw["fixture"], "custom_target_json.fixture")
    base_fixture = next(
        (
            fixture
            for fixture in cross_target_fixtures
            if fixture.target == custom_base_target
        ),
        None,
    )
    require(
        base_fixture is not None,
        "custom_target_json.base_target is not in cross_target_corpus",
    )
    require(
        base_fixture.fixture == custom_fixture,
        "custom_target_json.fixture disagrees with cross_target_corpus",
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
    custom_target = require_string(custom_raw["target"], "custom_target_json.target")
    custom_runner = require_string(custom_raw["runner"], "custom_target_json.runner")
    custom_host = next(
        (host for host in native_hosts if host.target == custom_target), None
    )
    require(
        custom_host is not None,
        "custom_target_json.target is not an advertised native host",
    )
    require(
        custom_host.runner == custom_runner,
        "custom_target_json.runner disagrees with its native host",
    )
    custom_target_json = CustomTargetJson(
        toolchain=custom_toolchain,
        target=custom_target,
        runner=custom_runner,
        base_target=custom_base_target,
        fixture=custom_fixture,
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
            setup in {"linux-tmpfs", "macos-apfs-case-sensitive", "windows-ntfs-vhd"},
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
            value, f"deferred_native_hosts[{index}]", {"name", "target"}
        )
        deferred_hosts.append(
            DeferredHost(
                name=require_string(
                    host["name"], f"deferred_native_hosts[{index}].name"
                ),
                target=require_string(
                    host["target"], f"deferred_native_hosts[{index}].target"
                ),
            )
        )
    require_unique_sorted(
        [host.target for host in deferred_hosts], "deferred_native_hosts targets"
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
        custom_target_json=custom_target_json,
        native_hosts=tuple(native_hosts),
        filesystem_profiles=tuple(filesystem_profiles),
        release_cross_targets=tuple(release_cross_targets),
        deferred_hosts=tuple(deferred_hosts),
        alternate_linkers=tuple(alternate_linkers),
        alternate_codegen_backends=tuple(alternate_backends),
    )


@dataclass(frozen=True)
class NativeCacheCertificate:
    platform: str
    host_target: str
    identity: str
    evidence: str


@dataclass(frozen=True)
class NativeCacheRegistry:
    schema_version: int
    cache_class: str
    execution_contract: str
    cargo_release: str
    rustc_release: str
    candidate_hosts: dict[str, str]
    certificates: tuple[NativeCacheCertificate, ...]

    def certified_targets(self) -> set[str]:
        return {certificate.host_target for certificate in self.certificates}


def required_source_value(source: str, pattern: str, name: str) -> str:
    matches = re.findall(pattern, source, flags=re.MULTILINE)
    require(len(matches) == 1, f"native-cache source must define exactly one {name}")
    return matches[0]


def load_native_cache_registry() -> NativeCacheRegistry:
    source_path = REPOSITORY_ROOT / "src/compiler/native_cache.rs"
    source = source_path.read_text(encoding="utf-8")
    registry_path = "distribution/native-cache-capabilities.json"
    require(
        f'include_str!("../../{registry_path}")' in source,
        "native-cache runtime does not embed the reviewed capability registry",
    )
    package_include = (
        load_toml(REPOSITORY_ROOT / "Cargo.toml").get("package", {}).get("include")
    )
    require(
        isinstance(package_include, list) and f"/{registry_path}" in package_include,
        "Cargo package include list is missing the native-cache capability registry",
    )
    cache_class = required_source_value(
        source,
        r'^const GRADUATED_NATIVE_CACHE_CLASS: &str = "([^"]+)";$',
        "GRADUATED_NATIVE_CACHE_CLASS",
    )
    rustc_release = required_source_value(
        source,
        r'^const GRADUATED_RUSTC_RELEASE: &str = "([^"]+)";$',
        "GRADUATED_RUSTC_RELEASE",
    )
    cargo_release = required_source_value(
        source,
        r'^const GRADUATED_CARGO_RELEASE: &str = "([^"]+)";$',
        "GRADUATED_CARGO_RELEASE",
    )
    execution_contract = required_source_value(
        source,
        r'^pub\(crate\) const DIRECT_EXECUTION_CONTRACT: &str = "([^"]+)";$',
        "DIRECT_EXECUTION_CONTRACT",
    )
    schema_version = int(
        required_source_value(
            source,
            r"^const NATIVE_CACHE_CAPABILITY_REGISTRY_VERSION: u32 = ([0-9]+);$",
            "NATIVE_CACHE_CAPABILITY_REGISTRY_VERSION",
        )
    )
    block_match = re.search(
        r"^const GRADUATED_NATIVE_HOSTS: &\[\(&str, &str\)\] = &\[\n(?P<body>.*?)^\];$",
        source,
        flags=re.MULTILINE | re.DOTALL,
    )
    require(
        block_match is not None,
        "native-cache source must define GRADUATED_NATIVE_HOSTS in the reviewed form",
    )
    candidate_hosts: dict[str, str] = {}
    platforms: set[str] = set()
    body = block_match.group("body")
    entries = re.findall(r'^  \("([^"]+)", "([^"]+)"\),$', body, flags=re.MULTILINE)
    recognized = (
        "\n".join(f'  ("{platform}", "{target}"),' for platform, target in entries)
        + "\n"
    )
    require(body == recognized, "GRADUATED_NATIVE_HOSTS contains an unrecognized entry")
    require(entries, "GRADUATED_NATIVE_HOSTS must not be empty")
    for platform, target in entries:
        require(platform not in platforms, f"duplicate graduated platform {platform}")
        require(
            target not in candidate_hosts, f"duplicate graduated host target {target}"
        )
        platforms.add(platform)
        candidate_hosts[target] = platform

    raw = require_object(
        load_json(REPOSITORY_ROOT / registry_path),
        "native-cache capability registry",
        {"schema_version", "class", "execution_contract", "certificates"},
    )
    require(
        raw["schema_version"] == schema_version,
        "native-cache capability registry schema does not match the runtime schema",
    )
    require(
        raw["class"] == cache_class,
        "native-cache capability registry class does not match the runtime class",
    )
    require(
        raw["execution_contract"] == execution_contract,
        "native-cache capability registry execution contract does not match the runtime contract",
    )
    require(
        isinstance(raw["certificates"], list),
        "native-cache capability certificates must be an array",
    )
    certificates: list[NativeCacheCertificate] = []
    certificate_keys: list[tuple[str, str]] = []
    for index, value in enumerate(raw["certificates"]):
        certificate = require_object(
            value,
            f"native-cache capability certificates[{index}]",
            {"platform", "host_target", "identity", "evidence"},
        )
        platform = require_string(
            certificate["platform"],
            f"native-cache capability certificates[{index}].platform",
        )
        host_target = require_string(
            certificate["host_target"],
            f"native-cache capability certificates[{index}].host_target",
        )
        identity = require_string(
            certificate["identity"],
            f"native-cache capability certificates[{index}].identity",
        )
        evidence = require_string(
            certificate["evidence"],
            f"native-cache capability certificates[{index}].evidence",
        )
        require(
            candidate_hosts.get(host_target) == platform,
            f"native-cache capability certificate {host_target} is outside the candidate host boundary",
        )
        require(
            re.fullmatch(r"sha256:[0-9a-f]{64}", identity) is not None,
            f"native-cache capability certificate {host_target} has an invalid identity",
        )
        certificate_keys.append((host_target, identity))
        certificates.append(
            NativeCacheCertificate(platform, host_target, identity, evidence)
        )
    require(
        certificate_keys == sorted(certificate_keys),
        "native-cache capability certificates must be sorted by host target and identity",
    )
    require(
        len(certificate_keys) == len(set(certificate_keys)),
        "native-cache capability certificates contain duplicate host/identity tuples",
    )

    run_source = (REPOSITORY_ROOT / "src/commands/run.rs").read_text(encoding="utf-8")
    require(
        "ActionKind::Build | ActionKind::Distribution" in run_source,
        "native cache must remain limited to build and distribution actions",
    )
    require(
        'std::env::var_os("CARGO_INCREMENTAL")' in run_source,
        "native cache must retain the explicit incremental policy gate",
    )
    bypass_source = source + (REPOSITORY_ROOT / "src/compiler/collector.rs").read_text(
        encoding="utf-8"
    )
    for reason in (
        "codegen_backend_not_graduated",
        "configured_linker_not_graduated",
        "cross_target_not_graduated",
        "custom_sysroot_not_graduated",
        "native_cache_capability_not_certified",
        "native_cache_capability_unavailable",
        "native_cache_platform_not_graduated",
        "native_cache_toolchain_incoherent",
    ):
        require(
            f'"{reason}"' in bypass_source,
            f"native-cache source is missing stable bypass reason {reason}",
        )

    return NativeCacheRegistry(
        schema_version=schema_version,
        cache_class=cache_class,
        execution_contract=execution_contract,
        cargo_release=cargo_release,
        rustc_release=rustc_release,
        candidate_hosts=candidate_hosts,
        certificates=tuple(certificates),
    )


@dataclass(frozen=True)
class Qualification:
    target: str
    workloads: tuple[str, ...]
    accepted_samples: int
    false_hits: int


@dataclass(frozen=True)
class QualificationRegistry:
    schema_version: int
    cache_class: str
    cargo_release: str
    rustc_release: str
    qualifications: dict[str, Qualification]


def load_qualifications() -> QualificationRegistry:
    raw = require_object(
        load_json(REPOSITORY_ROOT / "distribution/native-cache-qualifications.json"),
        "native-cache qualifications",
        {
            "schema_version",
            "class",
            "fixture",
            "cargo_release",
            "rustc_release",
            "qualifications",
        },
    )
    require(
        raw["schema_version"] == 1,
        "native-cache qualification schema_version must be 1",
    )
    fixture = require_string(raw["fixture"], "native-cache qualifications.fixture")
    require(
        (REPOSITORY_ROOT / fixture).is_dir(),
        "native-cache qualification fixture does not exist",
    )
    qualifications: dict[str, Qualification] = {}
    corpora: list[str] = []
    require(
        isinstance(raw["qualifications"], list),
        "native-cache qualifications must be an array",
    )
    targets: list[str] = []
    for index, value in enumerate(raw["qualifications"]):
        entry = require_object(
            value,
            f"native-cache qualifications[{index}]",
            {"target", "corpus", "workloads", "accepted_samples", "false_hits"},
        )
        target = require_string(
            entry["target"], f"native-cache qualifications[{index}].target"
        )
        corpora.append(
            require_string(
                entry["corpus"], f"native-cache qualifications[{index}].corpus"
            )
        )
        workloads = entry["workloads"]
        require(
            isinstance(workloads, list) and workloads,
            f"native-cache qualifications[{index}].workloads is empty",
        )
        require(
            all(isinstance(workload, str) and workload for workload in workloads),
            "qualification workloads are invalid",
        )
        require_unique_sorted(
            workloads, f"native-cache qualifications[{index}].workloads"
        )
        accepted_samples = entry["accepted_samples"]
        false_hits = entry["false_hits"]
        require(
            isinstance(accepted_samples, int)
            and not isinstance(accepted_samples, bool)
            and accepted_samples > 0,
            f"native-cache qualifications[{index}].accepted_samples must be positive",
        )
        require(false_hits == 0, f"native-cache qualifications[{index}] has false hits")
        targets.append(target)
        qualifications[target] = Qualification(
            target, tuple(workloads), accepted_samples, false_hits
        )
    require_unique_sorted(targets, "native-cache qualification targets")
    require(
        len(corpora) == len(set(corpora)),
        "native-cache qualification corpus IDs must be unique",
    )
    return QualificationRegistry(
        schema_version=raw["schema_version"],
        cache_class=require_string(raw["class"], "native-cache qualifications.class"),
        cargo_release=require_string(
            raw["cargo_release"], "native-cache qualifications.cargo_release"
        ),
        rustc_release=require_string(
            raw["rustc_release"], "native-cache qualifications.rustc_release"
        ),
        qualifications=qualifications,
    )


def validate_inventories(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheRegistry,
    qualifications: QualificationRegistry,
) -> None:
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
    toolchain_targets = toolchain.get("toolchain", {}).get("targets")
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

    require(
        set(native_cache.candidate_hosts) <= native_targets,
        "runtime native-cache candidate set includes a target outside the advertised native host set",
    )
    require(
        qualifications.cargo_release == native_cache.cargo_release
        and qualifications.rustc_release == native_cache.rustc_release,
        "performance qualification toolchain does not match the runtime graduation registry",
    )
    require(
        qualifications.cache_class == native_cache.cache_class,
        "performance qualification class does not match the runtime graduation registry",
    )
    require(
        set(qualifications.qualifications) <= set(native_cache.candidate_hosts),
        "performance qualification includes a target outside the runtime cache candidate set",
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
        "--forward-compatibility-matrix",
        "--linker-probes",
        "--codegen-backend-probes",
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


def workspace_msrv() -> RustRelease:
    manifest = load_toml(REPOSITORY_ROOT / "Cargo.toml")
    value = manifest.get("workspace", {}).get("package", {}).get("rust-version")
    require(
        isinstance(value, str),
        "Cargo.toml workspace.package.rust-version must be an exact string",
    )
    return RustRelease.parse(value, "Cargo.toml workspace.package.rust-version")


def stable_releases(current_stable: str) -> tuple[RustRelease, ...]:
    msrv = workspace_msrv()
    current = RustRelease.parse(current_stable, "current stable")
    require(
        msrv.major == current.major,
        "MSRV and current stable must have the same Rust major release",
    )
    require(
        current >= msrv, f"current stable {current} predates repository MSRV {msrv}"
    )

    releases = [msrv]
    releases.extend(
        RustRelease(msrv.major, minor, 0)
        for minor in range(msrv.minor + 1, current.minor)
    )
    if current != msrv:
        releases.append(current)
    return tuple(releases)


def expected_action_cache_state(
    target: str,
    rustc_release: str,
    native_cache: NativeCacheRegistry,
) -> str:
    if target not in native_cache.candidate_hosts:
        return "native_cache_platform_not_graduated"
    if (
        rustc_release != native_cache.rustc_release
        or rustc_release != native_cache.cargo_release
    ):
        return "native_cache_toolchain_not_graduated"
    if target not in native_cache.certified_targets():
        return "native_cache_capability_not_certified"
    return "active"


def compatibility_matrix(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheRegistry,
    current_stable: str,
) -> str:
    releases = stable_releases(current_stable)
    msrv = workspace_msrv()
    current = RustRelease.parse(current_stable, "current stable")
    include = []
    for host in manifest.native_hosts:
        for release in releases:
            release_text = str(release)
            include.append(
                {
                    "compatibility": {
                        "name": f"{host.target} / Rust {release_text}",
                        "target": host.target,
                        "runner": host.runner,
                        "cache-key": f"{host.cache_key}-rust-{release_text}",
                        "toolchain": "stable" if release == current else release_text,
                        "targets": ",".join(
                            fixture.target for fixture in manifest.cross_target_fixtures
                        ),
                        "release": release_text,
                        "full-suite": release in {msrv, current},
                        "selection-probes": release == current,
                        "cross-target-mutation-probes": release == current,
                        "linker-probes": release == current,
                        "codegen-backend-probes": False,
                        "filesystem": host.filesystem,
                        "case-sensitive": host.case_sensitive,
                        "incoherent-toolchain": (
                            str(msrv)
                            if release == current
                            and host.target == "x86_64-unknown-linux-gnu"
                            else ""
                        ),
                        "expected-cache-state": expected_action_cache_state(
                            host.target,
                            release_text,
                            native_cache,
                        ),
                    }
                }
            )
    return json.dumps({"include": include}, separators=(",", ":"))


def filesystem_matrix(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheRegistry,
    current_stable: str,
) -> str:
    release = str(RustRelease.parse(current_stable, "current stable"))
    include = [
        {
            "filesystem": {
                "name": profile.name,
                "target": profile.target,
                "runner": profile.runner,
                "setup": profile.setup,
                "kind": profile.filesystem,
                "case-sensitive": profile.case_sensitive,
                "toolchain": "stable",
                "release": release,
                "expected-cache-state": expected_action_cache_state(
                    profile.target,
                    release,
                    native_cache,
                ),
            }
        }
        for profile in manifest.filesystem_profiles
    ]
    return json.dumps({"include": include}, separators=(",", ":"))


def forward_compatibility_matrix(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheRegistry,
) -> str:
    target = "x86_64-unknown-linux-gnu"
    host = next(
        (
            candidate
            for candidate in manifest.native_hosts
            if candidate.target == target
        ),
        None,
    )
    require(host is not None, f"forward-compatibility host {target} is not advertised")
    include = [
        {
            "compatibility": {
                "name": f"{target} / Rust {channel}",
                "target": target,
                "runner": host.runner,
                "cache-key": f"{host.cache_key}-rust-{channel}",
                "toolchain": channel,
                "custom-target-json-probe": False,
                "linker-probes": False,
                "codegen-backend-probes": False,
                "expected-cache-state": expected_action_cache_state(
                    target, channel, native_cache
                ),
            }
        }
        for channel in ("beta", "nightly")
    ]
    custom = manifest.custom_target_json
    include.append(
        {
            "compatibility": {
                "name": f"{custom.target} / Rust {custom.toolchain} custom target JSON",
                "target": custom.target,
                "runner": custom.runner,
                "cache-key": f"{host.cache_key}-rust-{custom.toolchain}-custom-target",
                "toolchain": custom.toolchain,
                "custom-target-json-probe": True,
                "linker-probes": False,
                "codegen-backend-probes": True,
                "expected-cache-state": expected_action_cache_state(
                    custom.target,
                    custom.toolchain,
                    native_cache,
                ),
            }
        }
    )
    return json.dumps({"include": include}, separators=(",", ":"))


def formatted_list(values: tuple[str, ...]) -> str:
    return ", ".join(f"`{value}`" for value in values)


def render_markdown(
    manifest: CompatibilityManifest,
    native_cache: NativeCacheRegistry,
    qualifications: QualificationRegistry,
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
            cache = "Bypass: `cross_target_not_graduated`"
        else:
            execution = f"Advertised; full-suite CI required (`{host.runner}`)"
            cross = "—"
            release = "Native artifact required"
            if target in native_cache.certified_targets():
                certificate_count = sum(
                    certificate.host_target == target
                    for certificate in native_cache.certificates
                )
                cache = (
                    f"Graduated `{native_cache.cache_class}` "
                    f"(Cargo `{native_cache.cargo_release}`, rustc `{native_cache.rustc_release}`; "
                    f"{certificate_count} exact certificate"
                    f"{'' if certificate_count == 1 else 's'})"
                )
            elif target in native_cache.candidate_hosts:
                cache = "Bypass: `native_cache_capability_not_certified`"
            else:
                cache = "Bypass: `native_cache_platform_not_graduated`"
        qualification = qualifications.qualifications.get(target)
        if qualification is None:
            performance = "Not qualified"
        else:
            workloads = " + ".join(qualification.workloads)
            performance = (
                f"Qualified: {workloads}; {qualification.accepted_samples} accepted, "
                f"{qualification.false_hits} false hits"
            )
        rows.append(
            f"| `{target}` | {execution} | {cross} | {release} | {cache} | {performance} |"
        )

    deferred_rows = [
        (
            f"| {host.name} | `{host.target}` | Pass-through target execution remains unqualified | "
            "`native_cache_platform_not_graduated` | Not qualified |"
        )
        for host in manifest.deferred_hosts
    ]

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

    return f"""# Execution, Cache, and Performance Support

> Auto-generated from executable CI/release registries, native-cache production gates, and reviewed qualification
> manifests. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `{manifest.schema_version}`;
> native-cache capability registry schema: `{native_cache.schema_version}`; qualification schema:
> `{qualifications.schema_version}`.

Execution support, cache graduation, and performance qualification are independent. A cache bypass still executes
Cargo normally; it is not an execution-support failure.

Native-cache graduation is certificate-specific, not target-wide. Run `cargo rail doctor native-cache --format json`
to inspect the exact Cargo, rustc, rustdoc, sysroot, backend, host, and wrapper-protocol identity selected in the
captured workspace. A candidate host with no matching certificate executes normally with
`native_cache_capability_not_certified`.

## Host and target matrix

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache | Performance qualification |
|---|---|---|---|---|---|
{chr(10).join(rows)}

Linux musl rows are required release cross-builds. They are not native Linux host evidence.

## Filesystem matrix

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
{chr(10).join(filesystem_rows)}

Alternate filesystem profiles use bounded temporary volumes and must cleanly detach them even after a failed test.

## Deferred native hosts

| Platform | Target | Execution status | Cache status | Performance status |
|---|---|---|---|---|
{chr(10).join(deferred_rows)}

IBM Power and IBM Z need native runners before cargo-rail can advertise native compatibility, cache graduation, or
performance qualification. Their ordinary Cargo target requests remain fail-closed for reuse.

## Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | {linkers} | Current-stable native lanes prove the default, its explicit driver, and the bundled host LLD flavor as Cargo-owned pass-through execution. No alternate is advertised; selected linkers retain `configured_linker_not_graduated`. |
| Codegen backend | {backends} | Native lanes prove stable LLVM, and the pinned-nightly lane proves Cranelift plus unknown-backend diagnostics as rustc-owned pass-through execution. No alternate is advertised; selected backends retain `codegen_backend_not_graduated`. |

No non-default implementation becomes advertised merely because cargo-rail preserves its invocation. It first needs a
named compatibility fixture on every applicable native host.

## Cache layers

| Layer | Current support | Authority boundary |
|---|---|---|
| Compiler-evidence cache | Workspace-only `unify` observations with complete revalidation | Diagnostic evidence; never restores Cargo artifacts |
| Hermetic whole-action cache | Current-host macOS pure-Rust `cargo check` class | Verified action/result manifest and isolated output tree |
| Native compiler-result cache | Eligible library metadata/rlib invocations listed above | Verified per-invocation action/result binding through Cargo's wrapper boundary |

## Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Graduated only for listed host/toolchain tuples | One declared crate root, complete observed Rust inputs, dep-info, `.rmeta`, optional `.rlib`, Rust-only dependency artifacts, no linker responsibility |
| Incremental compilation | Reuse bypassed; compiler executes | Requires `CARGO_INCREMENTAL=0`; forced incremental compilation also bypasses |
| Binary, test, example, and benchmark linking | Reuse bypassed; compiler/linker executes | Linker-producing invocations are not graduated |
| `dylib`, `cdylib`, and `staticlib` | Reuse bypassed; compiler/linker executes | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Reuse bypassed; compiler executes | Compile-time filesystem/process reads are not completely observed |
| Build scripts and generated output | Reuse bypassed; build script executes | Normal Cargo messages do not prove the ordered instruction stream, runtime reads, generated tree, or freshness |
| Native dependencies and `links` contracts | Reuse bypassed; native tools execute | External compiler, archiver, linker, SDK, and discovery inputs are incomplete |
| rustdoc and doctests | Reuse bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree; doctest execution is separate |
| Cross compilation and custom target specifications | Reuse bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom compiler wrappers | Preserved; cargo-rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI `--config` and action-defined environments | Reuse bypassed; Cargo executes | Effective build configuration or environment is outside the graduated direct-action contract |

See [Caching](caching.md) for activation, telemetry, benchmark evidence, and the graduation rules behind this matrix.
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    output = parser.add_mutually_exclusive_group(required=True)
    output.add_argument("--github-matrix", action="store_true")
    output.add_argument("--compatibility-matrix", action="store_true")
    output.add_argument("--filesystem-matrix", action="store_true")
    output.add_argument("--forward-compatibility-matrix", action="store_true")
    output.add_argument("--markdown", action="store_true")
    parser.add_argument("--current-stable")
    args = parser.parse_args()

    try:
        manifest = load_compatibility_manifest()
        native_cache = load_native_cache_registry()
        qualifications = load_qualifications()
        validate_inventories(manifest, native_cache, qualifications)
        if args.github_matrix:
            print(github_matrix(manifest))
        elif args.compatibility_matrix:
            require(
                args.current_stable is not None,
                "--compatibility-matrix requires --current-stable",
            )
            print(compatibility_matrix(manifest, native_cache, args.current_stable))
        elif args.filesystem_matrix:
            require(
                args.current_stable is not None,
                "--filesystem-matrix requires --current-stable",
            )
            print(filesystem_matrix(manifest, native_cache, args.current_stable))
        elif args.forward_compatibility_matrix:
            require(
                args.current_stable is None,
                "--current-stable only applies to --compatibility-matrix",
            )
            print(forward_compatibility_matrix(manifest, native_cache))
        else:
            require(
                args.current_stable is None,
                "--current-stable only applies to --compatibility-matrix",
            )
            print(render_markdown(manifest, native_cache, qualifications), end="")
        return 0
    except (ContractError, OSError) as error:
        print(f"support-matrix: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
