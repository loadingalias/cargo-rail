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
            "advertised_non_default_linkers",
            "advertised_non_default_codegen_backends",
        },
    )
    require(
        raw["schema_version"] == 5, "compatibility manifest schema_version must be 5"
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
        "compiler_diagnostic_format_not_graduated",
        "compiler_flag_not_graduated",
        "configured_linker_not_graduated",
        "cross_target_not_graduated",
        "custom_sysroot_not_graduated",
        "native_cache_capability_unavailable",
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
        "--linker-probes",
        "--direct-repeatability-probe",
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
                "linker-probes": True,
                "direct-repeatability-probe": host.target.endswith("-pc-windows-msvc"),
                "filesystem": host.filesystem,
                "case-sensitive": host.case_sensitive,
                "expected-cache-state": "active",
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
                "expected-cache-state": "active",
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
            cache = "Bypass: `cross_target_not_graduated`"
        else:
            execution = f"Advertised; full-suite CI required (`{host.runner}`)"
            cross = "—"
            release = "Native artifact required"
            cache = (
                f"Active for structurally eligible `{native_cache.cache_class}` units; "
                "exact compiler identity is part of every key"
            )
        rows.append(
            f"| `{target}` | {execution} | {cross} | {release} | {cache} |"
        )

    deferred_rows = [
        (
            f"| {host.name} | `{host.target}` | Native execution evidence is not retained yet | "
            "Structurally active when the exact compiler identity is captured |"
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

    return f"""# Caching

> Auto-generated from executable CI/release registries and native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `{manifest.schema_version}`;
> native-cache compiler-identity schema: `{native_cache.schema_version}`.

Planning removes actions that do not need to run. Caching removes work from selected actions only when Cargo-Rail can
prove that the stored result still matches every relevant input.

| Cache | Purpose | Work skipped on a hit |
|---|---|---|
| Compiler evidence | Reuse `unify` observations after complete input revalidation | Workspace diagnostic collection |
| Hermetic whole action | Restore one eligible isolated Cargo check | Cargo and compiler work; the exact fast path also skips bootstrap |
| Native compiler result | Restore one eligible rustc result through Cargo's wrapper boundary | That rustc invocation |

The layers have separate eligibility. A lookup or Cargo `fresh` flag never authorizes reuse. Cargo-Rail revalidates the
input identity, action/result binding, manifest, and exact stored bytes owned by the layer. Incomplete evidence runs
cold with a stable reason. **Fast when proven. Normal Cargo when not.**

## Native compiler-result cache

Native reuse is automatic for an ordinary `build` or `distribution` action when all of these are true:

- Cargo-Rail can content-identify the exact Cargo, rustc, rustdoc, complete sysroot, host, and wrapper protocol;
- the selected Cargo profile has no active fingerprints and neither the environment nor rustc forces incremental
  compilation;
- no Cargo CLI `--config`, action-defined environment, unknown Cargo setting, `build.dep-info-basedir`, sccache, or
  custom compiler wrapper changes the boundary; and
- the invocation is an eligible dependency or workspace library whose complete bounded source namespace, exact
  compiler-visible environment, selected-input proof, metadata, optional rlib output, and Rust-only dependency
  artifacts remain inside the graduated class, with no linker responsibility.

```bash
cargo rail run --all --action build --explain
cargo rail run --all --action distribution --explain
cargo rail doctor native-cache --format json
```

Each command session validates one physical source root. Reusable action and result identities replace that root with
a versioned portable root, so equivalent source, toolchain, environment, dependency, target, and output evidence can
reuse across arbitrary checkout roots. Each compiler unit binds its exact rustc arguments and cfg, dependency artifact
contents, every entry and regular-file byte below the crate-root directory, and every compiler-visible environment
name and value after removing only Cargo-Rail's exact private controls. Rustc observation then proves that the
successful invocation selected no input outside those capabilities. The action identity is available before lookup;
lookup work does not grow with retained source history.

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
source and output roots, so eligible cold invocations use a versioned compiler remap. The CAS stores reversible tokens
for verified dep-info and JSON compiler-stream paths, including their Windows separator and escaping form, then binds
them to the current source and output roots after verification. Output names and materialized bytes remain exact;
ambiguous root spellings or unmodeled cached paths fail closed.
Cargo-Rail sets `CARGO_INCREMENTAL=0` only for an eligible clean-profile child. An active profile, an explicit nonzero
incremental request, or forced incremental compilation keeps Cargo's ordinary path. The doctor reports the exact
compiler identity without running a build. If that exact identity cannot be captured, Cargo executes normally with
`native_cache_capability_unavailable`. Existing wrappers remain in their selected order and Cargo-Rail does not add a
second cache.

For the exact normal all-workspace `build` and `distribution` shapes, an unambiguous active profile delegates the
unchanged built-in Cargo action before metadata, Git, tool hashing, and action-key construction. Cargo configuration
that makes the target location ambiguous, a non-workspace manifest, or an explicit incremental setting retains the
captured planner/runner path. An eligible clean profile uses one locked, no-dependencies Cargo metadata query to prove
the workspace-library boundary, then enters the same verified compiler-result cache without capturing Git state or
expanding the full action plan. Any ambiguity or acquisition failure falls back to the captured path. Both shortcuts
preserve ambient wrappers and record their deliberately absent snapshot in the ordinary decision receipt.

Default text mode emits one concise decision with `hits`, `misses`, `bypasses`, and `bytes_restored`, or the stable
action-level bypass reason. `--explain` adds `setup_bytes_hashed`, `bytes_hashed`, accounted verified-result
`cache_bytes_read` and `cache_bytes_written` totals, the complete reason census, and per-unit evidence. Low-level I/O
that fails without returning byte statistics is not inferred:

- `hit` means current inputs and every stored object were reverified before exact output bytes were restored;
- `miss` means the exact action has no authoritative result; successful cold output may populate the local CAS; and
- `bypass` means the session or invocation is outside the graduated class and rustc executed normally.

The corrected local store resolves an exact action to zero or one authoritative result. A second semantic result for
the same action becomes a durable conflict; malformed or transaction-ambiguous state becomes quarantined. Neither
state restores cached output. Restore uses one durable marker for the exact destination set. Failure before the first
visible replacement falls back to the original compiler; failure after that boundary cleans every owned destination
and fails without running rustc over a partial commit.

Corrupt or incompatible native entries never authorize reuse. Cargo still owns its fingerprints; Cargo-Rail never
restores a target directory or synthesizes Cargo freshness. Use `--no-cache` for an intentional cold baseline.

## Shared native cache (L2)

Select one machine target by alias; L1 remains enabled by default:

```toml
[cache]
l2 = "team"
```

An accepted L1 hit is network-free. On an L1 miss, the command-owned coordinator can stream one S3 result into private
L1 staging. The ordinary L1 proof must accept the imported result before restore. A `read_write` target publishes a
verified local pack before its action association. The compiler wrapper receives only a loopback capability, never S3
credentials or a bucket name.

Set `CARGO_RAIL_CACHE_TARGETS_FILE` to an absolute machine-owned JSON file outside the checkout. This is the complete
version 1 schema; unknown fields are rejected:

```json
{{
  "version": 1,
  "targets": {{
    "team": {{
      "protocol": "s3",
      "region": "us-east-1",
      "expected_bucket_owner": "123456789012",
      "bucket": "company-cargo-rail-cache",
      "prefix": "rust/team",
      "role": "read_write",
      "shareable_environment": ["CARGO", "CARGO_CRATE_NAME", "LANG", "PATH"]
    }}
  }}
}}
```

Aliases use lowercase ASCII letters, digits, `-`, and `_`, and start with a letter. `read` permits `GetObject` only;
`read_write` also permits conditional `PutObject`. Restrict the AWS principal to the selected bucket prefix.
`shareable_environment` is a sorted, unique list of non-secret compiler-environment names that may participate in L2
reuse. An action with any other selected environment name stays local.

Before granting client access, create `<prefix>/native-v3/protocol` with the exact 27-byte body
`cargo-rail-native-cache-v3\n`. Keep this marker and selector/action objects permanent; apply expiration only to
`<prefix>/native-v3/results/`. Require TLS and reject every `PutObject` that has neither `If-Match` nor
`If-None-Match`. Clients need no list, delete, ACL, bucket-policy, or lifecycle authority. The marker is required
because S3 reports a missing object as `403` when a caller has `GetObject` but not `ListBucket`; Cargo-Rail validates
the marker before treating `403` on a content-addressed cache key as a miss.

The command parent loads credentials from the standard AWS SDK chain. The target map contains no credentials.
Cargo-Rail pins the expected bucket owner and the AWS SDK's official regional HTTPS endpoint; custom endpoints and
S3-compatible services are not supported. Endpoint overrides, access points, multi-region access points, S3 Express,
FIPS, dual-stack, and accelerated endpoints are disabled or rejected.

Before starting Cargo, an L2-enabled command removes the target-map variable, AWS credential and provider-selector
variables, and AWS endpoint-override variables from the child environment. This prevents accidental inheritance; it
is not a sandbox. A same-UID build script or proc macro can still read default AWS credential files or contact a
workload metadata service. Run untrusted builds under a principal or container with no L2 credentials, or omit
`cache.l2`.

```bash
cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

`cache status` validates the selected target without resolving credentials. `doctor native-cache` resolves
credentials and validates the immutable protocol marker. A missing target, unavailable credentials, authentication error,
timeout, service failure, or remote miss compiles cold through L1 and opens one command-local remote circuit when
applicable. A remote conflict, malformed object, or action/result mismatch is an integrity failure and restores
nothing. Publication failure is reported after local admission and does not change successful compilation.

The S3 protocol and local coordinator are implemented, but live cross-host S3 reuse has not yet been retained as
release evidence.

## Hermetic whole-action cache

```bash
cargo rail run --all --action build --hermetic --explain
cargo rail run --all --action build --hermetic --no-cache
```

The graduated class is a pure-Rust, current-host Cargo check on macOS. A cold run requires an exact `Cargo.lock`,
performs one `cargo fetch --locked` network boundary, then runs locked and offline in fresh roots with read-only source
and dependency inputs.

The process-free lookup accepts `cargo rail run --all --action build --hermetic` in text mode, optionally with
`--explain` or `--print-cmd`, and no configuration override or trailing Cargo arguments. Its verified hit restores the
complete output manifest before workspace context, metadata, fetch, Cargo, or compiler processes start. Other requests
bootstrap normally before any action-cache decision.

Other hosts run the isolated check but report `platform_limited` and receive no action key until Cargo-Rail can enforce
an equivalent filesystem and network boundary. Build scripts, proc macros, documentation, linked or native artifacts,
cross targets, custom tools, configured wrappers, and unmodeled Cargo overrides fail closed for this profile.

`target/cargo-rail/hermetic/reports/` records support, enforcement, action and result identities, fetch reuse, outputs,
cache status, and stable reasons. A corrupt or incompatible whole-action entry fails rather than silently restoring;
use `--no-cache` for a deliberate cold run or validated cleanup to discard it.

## Compiler-evidence cache

`cargo rail unify --check` may reuse compiler observations after revalidating the compiler, source, manifest, target,
features, Cargo configuration, dependency artifacts, emitted outputs, executable identity, and recorded environment
reads. This store contains diagnostic evidence, not restorable Cargo artifacts.

Check mode does not edit manifests, but analysis may update cache and report files under `target/cargo-rail/`.
`cargo rail unify --check -f json` exposes `evidence_cache` hits, misses, and reasons.

## Storage and cleanup

The default local authority root is `$CARGO_HOME/cargo-rail/local-cas-v2` or
`$HOME/.cargo/cargo-rail/local-cas-v2`. `CARGO_RAIL_CACHE_DIR` selects another machine-authorized base;
`CARGO_RAIL_CACHE_MAX_BYTES` changes the positive 10 GiB result bound. The owner directory contains a generated opaque
trust-domain marker, and the CAS validates the matching root marker before every use. An explicit
`CARGO_RAIL_CACHE_TRUST_DOMAIN` selects `local-cas-v2-<id>` so protected CI, untrusted jobs, and managed interactive
workloads can use physically isolated roots. The variable names authority; it is not isolation by itself.

```bash
cargo rail cache status
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local
```

Workspace cleanup removes reconstructible state for the current checkout. Local cleanup resolves only the currently
selected trust domain, validates its owner marker, waits for in-flight readers, and removes that cross-workspace CAS.
A legacy `local-cas-v1` is reclaim-only and never becomes v2 authority. Use `--scope all` only when both effects are
intended. `cargo rail clean --cache` remains a combined compatibility
alias. Do not remove individual CAS objects or Cargo fingerprints by hand.

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

IBM Power and IBM Z need native runners before Cargo-Rail can claim tested execution. They are not excluded by a
runtime platform allowlist.

### Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | {linkers} | Default and bundled host linkers are tested. An explicitly configured linker keeps the complete action on Cargo's path until Cargo-Rail can preserve its exact argv and effects. |
| Codegen backend | {backends} | Bundled named backends are bound by the complete sysroot identity and compiler arguments. An external backend path bypasses until its executable bytes are content-identified. |

Pass-through execution is not cache graduation. A non-default implementation needs a named compatibility fixture on
every applicable native host before Cargo-Rail advertises it.

### Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Active for any exact, content-identified native toolchain | One live-root-bound session, one portable declared crate root, complete bounded source topology and bytes, exact compiler environment, containment observation, `.rmeta`, optional `.rlib`, Rust-only dependencies, no linker responsibility |
| Incremental compilation | Automatic clean-profile policy | Active fingerprints, explicit nonzero incremental requests, and forced incremental mode preserve Cargo's path; eligible clean profiles run non-incrementally without global setup |
| Binary, test, example, and benchmark linking | Bypassed; compiler/linker executes | Linker-producing invocations are not graduated |
| `dylib`, `cdylib`, and `staticlib` | Bypassed; compiler/linker executes | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Bypassed; compiler executes | Compile-time filesystem and process reads are not completely observed |
| Build scripts and generated output | Bypassed; build script executes | Cargo messages do not prove the ordered instruction stream, runtime reads, output tree, or freshness |
| Native dependencies and `links` contracts | Bypassed; native tools execute | External tools, headers, SDKs, libraries, discovery inputs, and outputs are incomplete |
| rustdoc and doctests | Bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree |
| Cross compilation and custom targets | Bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom wrappers | Preserved; Cargo-Rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI `--config` and action environments | Bypassed; Cargo executes | Effective configuration or environment is outside the graduated direct-action contract |

## Benchmark evidence

The fixture contains registry and Git dependencies, build scripts, a proc macro, native code, workspace libraries, and
a binary:

```bash
just bench-native-cache-smoke
just bench-native-cache 10
```

The v6 execution contract invalidates v5 measurements. V6 binds the command's effective default regular-file creation
mode (the Unix umask result) into session and action identity, and binds each exact output mode into result identity,
the canonical descriptor, and the result pack. Different effective creation modes cannot cross-hit. Do not compare
measurements from different execution contracts.

The benchmark now seeds and measures Cargo-Rail in one authoritative source root with a clean target directory and a
fresh copy of the populated CAS. Acceptance hashes every `.d`, `.rmeta`, and `.rlib` byte without a root-bound
exclusion, and requires identical action censuses, runtime behavior, compiler-event identities, cache accounting, and
measured/proof-replay outcomes. Specialist comparisons may still use distinct roots, but they do not measure
Cargo-Rail's arbitrary-root reuse path; the independent-root fixture carries that correctness proof.

When evaluating another workspace, record the repository commit, tool and host/target identities, linker, runner,
wrappers, flags, exact action argv, clean-root method, native/disabled/cold/warm timings, hit and byte counts, all bypass
reasons, and byte-identity findings. Choose the group count for the decision being made; one accepted interleaved group
is a correctness smoke test, while repeated groups support p50/p95 comparisons.
The benchmark pins the current stable `sccache` release from the CI tool registry and measures both its server and
opt-in client-side local-disk paths. Preserve raw output and never combine different commits, toolchains, targets,
policies, or cache modes.
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
