#!/usr/bin/env python3
"""Run the small cargo-rail front-door corpus with one selected Rust toolchain."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = REPOSITORY_ROOT / "tests/compatibility/fixtures/front-door"
MANIFEST_PATH = REPOSITORY_ROOT / "tests/compatibility/manifest.json"
PACKAGE = "cargo-rail-compatibility-fixture"
SCRUBBED_ENVIRONMENT = (
    "CARGO",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_RAIL_COMPAT_LINK_RESPONSE",
    "CARGO_TARGET_DIR",
    "LINK",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
    "SOURCE_DATE_EPOCH",
)


class CompatibilityError(RuntimeError):
    """One compatibility assertion failed."""


@dataclass(frozen=True)
class ProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes


@dataclass(frozen=True)
class ActionCase:
    name: str
    action: str
    cargo_argv: tuple[str, ...]
    rail_options: tuple[str, ...] = ()
    rail_run_args: tuple[str, ...] = ()
    rail_cargo_test_args: tuple[str, ...] = ()


@dataclass(frozen=True)
class CrossTargetCase:
    target: str
    fixture: Path
    artifact: str


def display_argv(argv: list[str]) -> str:
    return subprocess.list2cmdline(argv)


def run(
    argv: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    expected_codes: tuple[int, ...] | None = (0,),
) -> ProcessResult:
    completed = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, check=False)
    result = ProcessResult(completed.returncode, completed.stdout, completed.stderr)
    if expected_codes is not None and result.returncode not in expected_codes:
        raise CompatibilityError(
            f"command exited {result.returncode}, expected {expected_codes}: {display_argv(argv)}\n"
            f"stdout:\n{result.stdout.decode(errors='replace')}\n"
            f"stderr:\n{result.stderr.decode(errors='replace')}"
        )
    return result


def selected_environment(
    toolchain: str, cargo_home: Path, cache: Path
) -> dict[str, str]:
    environment = os.environ.copy()
    for name in SCRUBBED_ENVIRONMENT:
        environment.pop(name, None)
    environment.update(
        {
            "CARGO_BUILD_JOBS": "1",
            "CARGO_HOME": str(cargo_home),
            "CARGO_INCREMENTAL": "0",
            "CARGO_NET_OFFLINE": "true",
            "CARGO_RAIL_CACHE_DIR": str(cache),
            "CARGO_TERM_COLOR": "never",
            "RUSTUP_TOOLCHAIN": toolchain,
        }
    )
    return environment


def first_line(result: ProcessResult) -> str:
    return result.stdout.decode("utf-8").splitlines()[0]


def assert_selected_toolchain(
    toolchain: str,
    expected_release: str | None,
    expected_host: str,
    *,
    cwd: Path,
    env: dict[str, str],
) -> None:
    cargo = first_line(run(["cargo", "--version"], cwd=cwd, env=env))
    rustc = first_line(run(["rustc", "--version"], cwd=cwd, env=env))
    rustdoc = first_line(run(["rustdoc", "--version"], cwd=cwd, env=env))
    rustc_verbose = run(
        ["rustc", "--version", "--verbose"], cwd=cwd, env=env
    ).stdout.decode("utf-8")
    host = next(
        (
            line.removeprefix("host:").strip()
            for line in rustc_verbose.splitlines()
            if line.startswith("host:")
        ),
        "",
    )
    if host != expected_host:
        raise CompatibilityError(
            f"selected rustc host is {host!r}, expected {expected_host!r}"
        )

    if expected_release is not None:
        for program, value in (
            ("cargo", cargo),
            ("rustc", rustc),
            ("rustdoc", rustdoc),
        ):
            if not value.startswith(f"{program} {expected_release} "):
                raise CompatibilityError(
                    f"{program} did not resolve through selected Rust {expected_release}: {value}"
                )
    elif toolchain.startswith("nightly-"):
        active = first_line(
            run(["rustup", "show", "active-toolchain"], cwd=cwd, env=env)
        ).split()[0]
        if "nightly" not in rustc or not active.startswith(f"{toolchain}-"):
            raise CompatibilityError(
                f"rustc did not resolve through dated {toolchain}: {rustc}, active={active!r}"
            )
    elif toolchain not in rustc:
        raise CompatibilityError(
            f"rustc did not resolve through selected {toolchain} channel: {rustc}"
        )


def initialize_git_repository(destination: Path, env: dict[str, str]) -> None:
    git_environment = env.copy()
    git_environment.update(
        {
            "GIT_AUTHOR_EMAIL": "compatibility@example.invalid",
            "GIT_AUTHOR_NAME": "cargo-rail compatibility",
            "GIT_COMMITTER_EMAIL": "compatibility@example.invalid",
            "GIT_COMMITTER_NAME": "cargo-rail compatibility",
        }
    )
    run(["git", "init", "--quiet"], cwd=destination, env=git_environment)
    run(
        ["git", "config", "core.autocrlf", "false"],
        cwd=destination,
        env=git_environment,
    )
    run(["git", "add", "--all"], cwd=destination, env=git_environment)
    run(
        ["git", "commit", "--quiet", "-m", "compatibility baseline"],
        cwd=destination,
        env=git_environment,
    )


def initialize_workspace(destination: Path, env: dict[str, str]) -> None:
    shutil.copytree(FIXTURE_ROOT, destination)
    initialize_git_repository(destination, env)
    git_environment = env.copy()
    git_environment.update(
        {
            "GIT_AUTHOR_EMAIL": "compatibility@example.invalid",
            "GIT_AUTHOR_NAME": "cargo-rail compatibility",
            "GIT_COMMITTER_EMAIL": "compatibility@example.invalid",
            "GIT_COMMITTER_NAME": "cargo-rail compatibility",
        }
    )
    with (destination / "src/lib.rs").open(
        "a", encoding="utf-8", newline="\n"
    ) as source:
        source.write(
            "\n// The second commit gives the planner a deterministic changed source.\n"
        )
    run(["git", "add", "src/lib.rs"], cwd=destination, env=git_environment)
    run(
        ["git", "commit", "--quiet", "-m", "exercise planner"],
        cwd=destination,
        env=git_environment,
    )


def cross_target_cases() -> tuple[CrossTargetCase, ...]:
    try:
        manifest = json.loads(MANIFEST_PATH.read_bytes())
        raw_cases = manifest["cross_target_corpus"]
        cases = tuple(
            CrossTargetCase(
                target=entry["target"],
                fixture=REPOSITORY_ROOT / entry["fixture"],
                artifact=entry["artifact"],
            )
            for entry in raw_cases
        )
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(
            f"cross-target manifest is invalid: {error}"
        ) from error
    if not cases:
        raise CompatibilityError("cross-target manifest is empty")
    for case in cases:
        if not case.fixture.joinpath("Cargo.toml").is_file() or case.artifact not in {
            "rlib",
            "wasm",
        }:
            raise CompatibilityError(
                f"cross-target manifest entry is invalid: {case!r}"
            )
    return cases


def assert_plan(cargo_rail: Path, workspace: Path, env: dict[str, str]) -> None:
    result = run(
        [str(cargo_rail), "rail", "plan", "--since", "HEAD^", "--format", "json"],
        cwd=workspace,
        env=env,
    )
    try:
        plan = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise CompatibilityError(
            f"plan stdout is not one JSON value: {error}"
        ) from error
    surfaces = plan.get("surfaces", {})
    if (
        plan.get("command") != "plan"
        or plan.get("result") != "success"
        or not surfaces.get("build", {}).get("enabled")
        or not surfaces.get("test", {}).get("enabled")
    ):
        raise CompatibilityError(
            f"plan did not select build and test for the changed fixture: {plan!r}"
        )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(128 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_cargo_rustc_info(path: Path) -> str:
    try:
        value = json.loads(path.read_bytes())
    except json.JSONDecodeError as error:
        raise CompatibilityError(
            f"Cargo rustc-info cache is not valid JSON: {error}"
        ) from error
    if (
        not isinstance(value, dict)
        or set(value) != {"rustc_fingerprint", "outputs", "successes"}
        or not isinstance(value["outputs"], dict)
        or not isinstance(value["successes"], dict)
    ):
        raise CompatibilityError("Cargo rustc-info cache has an unrecognized shape")
    # Cargo deliberately binds the map keys and rustc_fingerprint to its
    # configured wrapper. The probe results are the semantic toolchain data.
    # Preserve and compare every result and the successes map; discard only
    # those wrapper-derived lookup hashes.
    semantic = {
        "outputs": sorted(
            value["outputs"].values(),
            key=lambda output: json.dumps(
                output, sort_keys=True, separators=(",", ":")
            ),
        ),
        "successes": value["successes"],
    }
    encoded = json.dumps(semantic, sort_keys=True, separators=(",", ":")).encode(
        "utf-8"
    )
    return hashlib.sha256(encoded).hexdigest()


def output_manifest(
    root: Path, *, allow_empty: bool = False
) -> tuple[tuple[str, str, int, str], ...]:
    entries: list[tuple[str, str, int, str]] = []
    if not root.exists():
        if allow_empty:
            return ()
        raise CompatibilityError(f"Cargo produced no target directory at {root}")
    for path in sorted(
        root.rglob("*"), key=lambda candidate: candidate.relative_to(root).as_posix()
    ):
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        mode = stat.S_IMODE(metadata.st_mode)
        if path.is_symlink():
            entries.append((relative, "symlink", mode, os.readlink(path)))
        elif path.is_dir():
            entries.append((relative, "directory", mode, ""))
        elif path.is_file():
            # Cargo binds this private probe cache's lookup hashes to its
            # configured rustc-wrapper. Compare its complete semantic probe
            # results. MSVC emits byte- and allocation-size-variant PDBs based
            # on prior compiler activity even with /Brepro. Require the native
            # MSF 7 container at its exact path and mode; every deterministic
            # compiler artifact remains byte-for-byte.
            if relative == ".rustc_info.json":
                digest = canonical_cargo_rustc_info(path)
            elif os.name == "nt" and path.suffix.casefold() == ".pdb":
                with path.open("rb") as pdb:
                    header = pdb.read(32)
                if not header.startswith(b"Microsoft C/C++ MSF 7.00"):
                    raise CompatibilityError(f"invalid native MSVC PDB at {path}")
                digest = "msvc-pdb-msf7-nondeterministic"
            else:
                digest = sha256(path)
            entries.append((relative, "file", mode, digest))
        else:
            raise CompatibilityError(f"unsupported output kind at {path}")
    if not entries and not allow_empty:
        raise CompatibilityError("Cargo produced an empty target directory")
    return tuple(entries)


def manifest_difference(
    expected: tuple[tuple[str, str, int, str], ...],
    actual: tuple[tuple[str, str, int, str], ...],
) -> str:
    expected_by_path = {entry[0]: entry[1:] for entry in expected}
    actual_by_path = {entry[0]: entry[1:] for entry in actual}
    lines = []
    for path in sorted(expected_by_path.keys() | actual_by_path.keys()):
        if path not in actual_by_path:
            lines.append(f"missing {path}: {expected_by_path[path]!r}")
        elif path not in expected_by_path:
            lines.append(f"extra {path}: {actual_by_path[path]!r}")
        elif expected_by_path[path] != actual_by_path[path]:
            lines.append(
                f"changed {path}: expected {expected_by_path[path]!r}, actual {actual_by_path[path]!r}"
            )
    if len(lines) > 20:
        lines = [*lines[:20], f"... {len(lines) - 20} more differences"]
    return "\n".join(lines)


def clean_directory(path: Path) -> None:
    if not path.exists():
        return

    def make_writable_and_retry(function: Any, value: str, _error: Any) -> None:
        os.chmod(value, stat.S_IWRITE)
        function(value)

    shutil.rmtree(path, onerror=make_writable_and_retry)


def encoded_rmeta_outputs(root: Path) -> dict[str, str]:
    return {
        path.relative_to(root).as_posix(): base64.b64encode(path.read_bytes()).decode(
            "ascii"
        )
        for path in root.rglob("*.rmeta")
    }


def rail_argv(
    cargo_rail: Path,
    case: ActionCase,
    target: Path,
    *,
    no_cache: bool,
    explain: bool,
    dry_run_json: bool,
) -> list[str]:
    argv = [
        str(cargo_rail),
        "rail",
        "run",
        "--quiet",
        "--all",
        "--action",
        case.action,
        *case.rail_options,
    ]
    if no_cache:
        argv.append("--no-cache")
    if explain:
        argv.append("--explain")
    if dry_run_json:
        argv.extend(["--dry-run", "--format", "json"])
    if case.rail_cargo_test_args:
        cargo_test_args = [*case.rail_cargo_test_args, "--target-dir", str(target)]
        argv.extend(f"--cargo-test-arg={argument}" for argument in cargo_test_args)
    else:
        argv.extend(["--", *case.rail_run_args, "--target-dir", str(target)])
    return argv


def direct_argv(case: ActionCase, target: Path) -> list[str]:
    return [*case.cargo_argv, "--target-dir", str(target)]


def assert_expanded_argv(
    cargo_rail: Path,
    case: ActionCase,
    target: Path,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    result = run(
        rail_argv(
            cargo_rail, case, target, no_cache=False, explain=False, dry_run_json=True
        ),
        cwd=workspace,
        env=env,
    )
    try:
        output = json.loads(result.stdout)
        actions = output["actions"]
        actual = actions[0]["argv"]
    except (json.JSONDecodeError, KeyError, IndexError, TypeError) as error:
        raise CompatibilityError(
            f"run dry-run stdout does not contain one action argv: {error}"
        ) from error
    expected = direct_argv(case, target)
    if len(actions) != 1 or actual != expected:
        raise CompatibilityError(
            f"{case.name} argv changed:\nexpected {expected!r}\nactual   {actual!r}"
        )


def execute_case(
    cargo_rail: Path,
    case: ActionCase,
    target: Path,
    *,
    workspace: Path,
    env: dict[str, str],
    verify_direct_repeatability: bool = False,
) -> tuple[ProcessResult, tuple[tuple[str, str, int, str], ...]]:
    clean_directory(target)
    direct = run(direct_argv(case, target), cwd=workspace, env=env)
    direct_outputs = output_manifest(target)
    direct_rmeta = (
        encoded_rmeta_outputs(target) if verify_direct_repeatability else {}
    )

    if verify_direct_repeatability:
        clean_directory(target)
        repeated = run(direct_argv(case, target), cwd=workspace, env=env)
        if repeated != direct:
            raise CompatibilityError(
                f"{case.name} repeated direct Cargo process result differs\n"
                f"first={direct!r}\nsecond={repeated!r}"
            )
        repeated_outputs = output_manifest(target)
        if repeated_outputs != direct_outputs:
            raise CompatibilityError(
                f"{case.name} repeated direct Cargo output inventory or bytes differ:\n"
                f"{manifest_difference(direct_outputs, repeated_outputs)}"
            )

    for label, no_cache in (("cache-disabled", True), ("cache-requested", False)):
        clean_directory(target)
        result = run(
            rail_argv(
                cargo_rail,
                case,
                target,
                no_cache=no_cache,
                explain=False,
                dry_run_json=False,
            ),
            cwd=workspace,
            env=env,
        )
        if result != direct:
            raise CompatibilityError(
                f"{case.name} {label} process result differs from direct Cargo\n"
                f"direct={direct!r}\nrail={result!r}"
            )
        outputs = output_manifest(target)
        if outputs != direct_outputs:
            rmeta_detail = ""
            if verify_direct_repeatability:
                rmeta_detail = (
                    f"\ndirect .rmeta base64={direct_rmeta!r}"
                    f"\n{label} .rmeta base64={encoded_rmeta_outputs(target)!r}"
                )
            raise CompatibilityError(
                f"{case.name} {label} output inventory or bytes differ from direct Cargo:\n"
                f"{manifest_difference(direct_outputs, outputs)}{rmeta_detail}"
            )
    return direct, direct_outputs


def resolve_expected_cache_state(
    cargo_rail: Path,
    expected: str,
    expected_host: str,
    *,
    workspace: Path,
    env: dict[str, str],
) -> str:
    if expected != "exact_certificate":
        return expected

    result = run(
        [str(cargo_rail), "rail", "doctor", "native-cache", "--format", "json"],
        cwd=workspace,
        env=env,
    )
    try:
        report = json.loads(result.stdout)
        capability = report["capability"]
        registry = json.loads(
            (
                REPOSITORY_ROOT / "distribution/native-cache-capabilities.json"
            ).read_bytes()
        )
        matches = [
            certificate
            for certificate in registry["certificates"]
            if certificate["platform"] == capability["platform"]
            and certificate["host_target"] == capability["host_target"]
            and certificate["identity"] == capability["identity"]
        ]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(
            f"native-cache capability report or registry is invalid: {error}"
        ) from error

    if (
        report.get("command") != "doctor"
        or report.get("mode") != "native_cache"
        or report.get("result") != "success"
        or report.get("exit_code") != 0
        or capability.get("host_target") != expected_host
        or capability.get("schema_version") != registry.get("schema_version")
        or capability.get("cache_class") != registry.get("class")
        or capability.get("execution_contract") != registry.get(
            "execution_contract"
        )
        or len(matches) > 1
    ):
        raise CompatibilityError(
            "native-cache capability report disagrees with the reviewed registry"
        )

    if matches:
        evidence = matches[0]["evidence"]
        if capability.get("certified") is not True or capability.get(
            "evidence"
        ) != evidence:
            raise CompatibilityError(
                "native-cache capability matches the registry but is not certified"
            )
        return "active"

    if capability.get("certified") is not False or capability.get(
        "evidence"
    ) is not None:
        raise CompatibilityError(
            "native-cache capability is absent from the registry but reports certification"
        )
    return "native_cache_capability_not_certified"


def assert_cache_explanation(
    cargo_rail: Path,
    case: ActionCase,
    target: Path,
    expected_cache_state: str,
    direct: ProcessResult,
    direct_outputs: tuple[tuple[str, str, int, str], ...],
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    clean_directory(target)
    explained = run(
        rail_argv(
            cargo_rail, case, target, no_cache=False, explain=True, dry_run_json=False
        ),
        cwd=workspace,
        env=env,
    )
    if explained.returncode != direct.returncode or explained.stderr != direct.stderr:
        raise CompatibilityError(f"{case.name} explain changed exit status or stderr")
    explained_outputs = output_manifest(target)
    if explained_outputs != direct_outputs:
        raise CompatibilityError(
            f"{case.name} explain changed output inventory or bytes:\n"
            f"{manifest_difference(direct_outputs, explained_outputs)}"
        )
    stdout = explained.stdout.decode("utf-8")
    marker = "native compiler cache:"
    if expected_cache_state == "active":
        if marker not in stdout or f"{marker} bypassed" in stdout:
            raise CompatibilityError(
                f"{case.name} did not report an active native cache:\n{stdout}"
            )
    else:
        expected = f"{marker} bypassed ({expected_cache_state})"
        if expected not in stdout:
            raise CompatibilityError(
                f"{case.name} did not report {expected_cache_state}:\n{stdout}"
            )


def assert_explicit_bypass_explanation(
    cargo_rail: Path,
    case: ActionCase,
    target: Path,
    expected_reasons: tuple[str, ...],
    direct: ProcessResult,
    direct_outputs: tuple[tuple[str, str, int, str], ...],
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    clean_directory(target)
    explained = run(
        rail_argv(
            cargo_rail, case, target, no_cache=False, explain=True, dry_run_json=False
        ),
        cwd=workspace,
        env=env,
    )
    if explained.returncode != direct.returncode or explained.stderr != direct.stderr:
        raise CompatibilityError(f"{case.name} explain changed exit status or stderr")
    explained_outputs = output_manifest(target)
    if explained_outputs != direct_outputs:
        raise CompatibilityError(
            f"{case.name} explain changed output inventory or bytes:\n"
            f"{manifest_difference(direct_outputs, explained_outputs)}"
        )

    prefix = "native compiler cache event: "
    events = []
    for line in explained.stdout.decode("utf-8").splitlines():
        if prefix not in line:
            continue
        try:
            events.append(json.loads(line.split(prefix, 1)[1]))
        except json.JSONDecodeError as error:
            raise CompatibilityError(
                f"{case.name} emitted a malformed cache event: {error}"
            ) from error
    stdout = explained.stdout.decode("utf-8")
    action_reason = any(
        f"native compiler cache: bypassed ({reason})" in stdout
        for reason in expected_reasons
    )
    event_reason = any(event.get("reason") in expected_reasons for event in events)
    if not action_reason and not event_reason:
        raise CompatibilityError(
            f"{case.name} did not report one of {expected_reasons!r}:\n{stdout}"
        )
    unexpected = [event for event in events if event.get("outcome") != "bypassed"]
    if unexpected:
        raise CompatibilityError(
            f"{case.name} authorized a cross-target cache outcome: {unexpected!r}"
        )


def assert_release_binary(target: Path, workspace: Path, env: dict[str, str]) -> None:
    executable = target / "release" / f"{PACKAGE}{'.exe' if os.name == 'nt' else ''}"
    result = run([str(executable)], cwd=workspace, env=env)
    if result.stdout != b"cargo-rail compatibility: 42\n" or result.stderr:
        raise CompatibilityError(f"release fixture output changed: {result!r}")


def assert_cross_target_artifact(target: Path, case: CrossTargetCase) -> None:
    release = target / case.target / "release"
    pattern = "*.wasm" if case.artifact == "wasm" else "*.rlib"
    artifacts = sorted(release.glob(pattern))
    if len(artifacts) != 1:
        raise CompatibilityError(
            f"{case.target} produced {len(artifacts)} top-level {case.artifact} artifacts: {artifacts!r}"
        )
    if case.artifact == "wasm" and artifacts[0].read_bytes()[:4] != b"\0asm":
        raise CompatibilityError(
            f"{case.target} output is not a WebAssembly module: {artifacts[0]}"
        )


def assert_cross_target_corpus(
    cargo_rail: Path,
    root: Path,
    *,
    action_cache_state: str,
    env: dict[str, str],
) -> None:
    expected_bypass_reasons = ("cross_target_not_graduated",)
    if action_cache_state != "active":
        expected_bypass_reasons += (action_cache_state,)
    for cross_target in cross_target_cases():
        workspace = root / f"workspace-{cross_target.target}"
        target = root / f"target-{cross_target.target}"
        shutil.copytree(cross_target.fixture, workspace)
        initialize_git_repository(workspace, env)
        case = ActionCase(
            name=cross_target.target,
            action="distribution",
            cargo_argv=(
                "cargo",
                "build",
                "--workspace",
                "--release",
                "--locked",
                "--quiet",
                "--target",
                cross_target.target,
            ),
            rail_run_args=("--quiet", "--target", cross_target.target),
        )
        assert_expanded_argv(cargo_rail, case, target, workspace=workspace, env=env)
        direct, direct_outputs = execute_case(
            cargo_rail,
            case,
            target,
            workspace=workspace,
            env=env,
        )
        assert_cross_target_artifact(target, cross_target)
        assert_explicit_bypass_explanation(
            cargo_rail,
            case,
            target,
            expected_bypass_reasons,
            direct,
            direct_outputs,
            workspace=workspace,
            env=env,
        )


def assert_cross_target_mutations(
    cargo_rail: Path,
    root: Path,
    expected_host: str,
    *,
    action_cache_state: str,
    env: dict[str, str],
) -> None:
    fixtures = {case.target: case.fixture for case in cross_target_cases()}

    sysroot = Path(
        run(["rustc", "--print=sysroot"], cwd=root, env=env)
        .stdout.decode("utf-8")
        .strip()
    )
    sysroot_alias = root / "cross target sysroot"
    create_sysroot_alias(sysroot, sysroot_alias, root, env)
    linker = (
        sysroot
        / "lib"
        / "rustlib"
        / expected_host
        / "bin"
        / ("rust-lld.exe" if os.name == "nt" else "rust-lld")
    )
    if not linker.is_file():
        raise CompatibilityError(
            f"selected toolchain does not contain rust-lld: {linker}"
        )

    probes: tuple[
        tuple[
            str,
            str,
            tuple[str, ...],
            dict[str, str],
            tuple[str, ...],
            tuple[str, str] | None,
        ],
        ...,
    ] = (
        (
            "source",
            "wasm32v1-none",
            (),
            env,
            ("cross_target_not_graduated",),
            ("src/lib.rs", "const REPEAT: usize = 2;"),
        ),
        (
            "features",
            "wasm32v1-none",
            ("--features", "variant"),
            env,
            ("cross_target_not_graduated",),
            None,
        ),
        (
            "target-flags",
            "wasm32v1-none",
            (),
            {**env, "CARGO_ENCODED_RUSTFLAGS": "-Ccodegen-units=1"},
            ("compiler_flag_not_graduated", "cross_target_not_graduated"),
            None,
        ),
        (
            "panic-mode",
            "wasm32v1-none",
            (),
            {**env, "CARGO_PROFILE_RELEASE_PANIC": "abort"},
            ("cross_target_not_graduated",),
            None,
        ),
        (
            "target-features",
            "wasm32-unknown-unknown",
            (),
            {**env, "CARGO_ENCODED_RUSTFLAGS": "-Ctarget-feature=+bulk-memory"},
            ("compiler_flag_not_graduated", "cross_target_not_graduated"),
            None,
        ),
        (
            "sysroot",
            "wasm32v1-none",
            (),
            {**env, "CARGO_ENCODED_RUSTFLAGS": f"--sysroot={sysroot_alias}"},
            ("custom_sysroot_not_graduated",),
            None,
        ),
        (
            "linker",
            "wasm32-unknown-unknown",
            (),
            {**env, "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER": str(linker)},
            ("configured_linker_not_graduated", "cross_target_not_graduated"),
            None,
        ),
    )

    for (
        label,
        target_name,
        cargo_arguments,
        probe_env,
        reasons,
        source_mutation,
    ) in probes:
        workspace = root / f"mutation-workspace-{label}"
        target = root / f"mutation-target-{label}"
        shutil.copytree(fixtures[target_name], workspace)
        if source_mutation is not None:
            relative, old = source_mutation
            source = workspace / relative
            contents = source.read_text(encoding="utf-8")
            if old not in contents:
                raise CompatibilityError(
                    f"{label} mutation input disappeared from {relative}"
                )
            source.write_text(
                contents.replace(old, "const REPEAT: usize = 4;", 1), encoding="utf-8"
            )
        initialize_git_repository(workspace, probe_env)

        case = ActionCase(
            name=f"{target_name} {label}",
            action="distribution",
            cargo_argv=(
                "cargo",
                "build",
                "--workspace",
                "--release",
                "--locked",
                "--quiet",
                "--target",
                target_name,
                *cargo_arguments,
            ),
            rail_run_args=("--quiet", "--target", target_name, *cargo_arguments),
        )
        assert_expanded_argv(
            cargo_rail, case, target, workspace=workspace, env=probe_env
        )
        direct, direct_outputs = execute_case(
            cargo_rail,
            case,
            target,
            workspace=workspace,
            env=probe_env,
        )
        expected_reasons = reasons
        if action_cache_state != "active":
            expected_reasons += (action_cache_state,)
        assert_explicit_bypass_explanation(
            cargo_rail,
            case,
            target,
            expected_reasons,
            direct,
            direct_outputs,
            workspace=workspace,
            env=probe_env,
        )


def assert_custom_target_json(
    cargo_rail: Path,
    root: Path,
    *,
    env: dict[str, str],
) -> None:
    try:
        manifest = json.loads(MANIFEST_PATH.read_bytes())
        configuration = manifest["custom_target_json"]
        base_target = configuration["base_target"]
        fixture = REPOSITORY_ROOT / configuration["fixture"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(
            f"custom-target manifest is invalid: {error}"
        ) from error

    workspace = root / "custom-target-workspace"
    target = root / "custom-target-output"
    spec = root / "cargo-rail-custom-target.json"
    shutil.copytree(fixture, workspace)
    initialize_git_repository(workspace, env)
    target_spec = run(
        [
            "rustc",
            "-Z",
            "unstable-options",
            "--print",
            "target-spec-json",
            "--target",
            base_target,
        ],
        cwd=workspace,
        env=env,
    )
    spec.write_bytes(target_spec.stdout)

    probe_env = env.copy()
    probe_env.pop("CARGO_NET_OFFLINE", None)
    case = ActionCase(
        name="custom target JSON",
        action="distribution",
        cargo_argv=(
            "cargo",
            "build",
            "--workspace",
            "--release",
            "--locked",
            "--quiet",
            "-Z",
            "json-target-spec",
            "-Z",
            "build-std=core",
            "--target",
            str(spec),
        ),
        rail_run_args=(
            "--quiet",
            "-Z",
            "json-target-spec",
            "-Z",
            "build-std=core",
            "--target",
            str(spec),
        ),
    )
    assert_expanded_argv(cargo_rail, case, target, workspace=workspace, env=probe_env)
    direct, direct_outputs = execute_case(
        cargo_rail,
        case,
        target,
        workspace=workspace,
        env=probe_env,
    )
    wasm = [
        artifact
        for artifact in target.rglob("*.wasm")
        if artifact.read_bytes()[:4] == b"\0asm"
    ]
    if not wasm:
        raise CompatibilityError("custom target JSON produced no WebAssembly module")
    assert_explicit_bypass_explanation(
        cargo_rail,
        case,
        target,
        (
            "native_cache_toolchain_not_graduated",
            "custom_target_not_graduated",
            "compiler_flag_not_graduated",
        ),
        direct,
        direct_outputs,
        workspace=workspace,
        env=probe_env,
    )


def msvc_linker(expected_host: str, workspace: Path, env: dict[str, str]) -> Path:
    target_architecture = "arm64" if expected_host.startswith("aarch64-") else "x64"
    def environment_value(name: str) -> str | None:
        return next(
            (value for key, value in env.items() if key.casefold() == name.casefold()),
            None,
        )

    program_files = (
        environment_value("ProgramFiles(x86)"),
        environment_value("ProgramFiles"),
    )
    vswhere = next(
        (
            Path(root) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
            for root in program_files
            if root is not None
            and (
                Path(root) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
            ).is_file()
        ),
        None,
    )
    if vswhere is None:
        raise CompatibilityError("Visual Studio locator vswhere.exe is unavailable")
    result = run(
        [
            str(vswhere),
            "-latest",
            "-products",
            "*",
            "-find",
            f"VC\\Tools\\MSVC\\**\\bin\\Host*\\{target_architecture}\\link.exe",
        ],
        cwd=workspace,
        env=env,
    )
    candidates = [
        Path(line.strip())
        for line in result.stdout.decode(errors="replace").splitlines()
        if line.strip()
    ]
    native_host = f"host{target_architecture}"
    candidates.sort(
        key=lambda candidate: (
            native_host not in str(candidate).casefold(),
            str(candidate).casefold(),
        )
    )
    if not candidates or not candidates[0].is_file():
        raise CompatibilityError(
            f"Visual Studio does not contain an MSVC linker for {target_architecture}"
        )
    return candidates[0]


def native_linker_tools(
    expected_host: str, workspace: Path, env: dict[str, str]
) -> tuple[Path, Path, str, str, str]:
    if expected_host.endswith("-apple-darwin"):
        driver_name = "clang"
        lld_flavor = "ld64.lld"
        driver_response_argument = "-Wl,-dead_strip"
        lld_response_argument = "-dead_strip"
    elif expected_host.endswith("-pc-windows-msvc"):
        driver_name = "link.exe"
        lld_flavor = "lld-link"
        driver_response_argument = "/OPT:REF"
        lld_response_argument = "/OPT:REF"
    elif expected_host.endswith("-unknown-linux-gnu"):
        driver_name = "cc"
        lld_flavor = "ld.lld"
        driver_response_argument = "-Wl,--gc-sections"
        lld_response_argument = "--gc-sections"
    else:
        raise CompatibilityError(
            f"no native linker probe contract exists for {expected_host}"
        )

    if expected_host.endswith("-pc-windows-msvc"):
        driver = msvc_linker(expected_host, workspace, env)
    else:
        resolved_driver = shutil.which(driver_name, path=env.get("PATH"))
        if resolved_driver is None:
            raise CompatibilityError(
                f"native linker driver {driver_name!r} is unavailable for {expected_host}"
            )
        driver = Path(resolved_driver)
    sysroot = Path(
        run(["rustc", "--print=sysroot"], cwd=workspace, env=env)
        .stdout.decode("utf-8")
        .strip()
    )
    rust_lld = (
        sysroot
        / "lib"
        / "rustlib"
        / expected_host
        / "bin"
        / ("rust-lld.exe" if os.name == "nt" else "rust-lld")
    )
    if not rust_lld.is_file():
        raise CompatibilityError(
            f"selected toolchain does not contain bundled rust-lld: {rust_lld}"
        )
    return (
        driver,
        rust_lld,
        lld_flavor,
        driver_response_argument,
        lld_response_argument,
    )


def assert_native_linker_probes(
    cargo_rail: Path,
    root: Path,
    expected_host: str,
    *,
    env: dict[str, str],
) -> None:
    (
        driver,
        rust_lld,
        lld_flavor,
        driver_response_argument,
        lld_response_argument,
    ) = native_linker_tools(expected_host, root, env)
    target_linker_environment = (
        "CARGO_TARGET_"
        + "".join(
            character.upper() if character.isalnum() else "_"
            for character in expected_host
        )
        + "_LINKER"
    )
    poison_linker = root / "linker must not run"
    lld_label = f"bundled rust-lld ({lld_flavor})"
    lld_encoded_rustflags = f"-Clinker={rust_lld}\x1f-Clinker-flavor={lld_flavor}"
    if expected_host.endswith("-pc-windows-msvc"):
        # lld-link generates a fresh PDB identity on every otherwise identical
        # clean link. This probe already exercises normal MSVC debug products;
        # suppress them here so the alternate-linker artifact stays byte-exact.
        lld_encoded_rustflags += "\x1f-Clink-arg=/Brepro\x1f-Clink-arg=/debug:none"
    lld_overrides = {"CARGO_ENCODED_RUSTFLAGS": lld_encoded_rustflags}
    lld_probe_response_argument = lld_response_argument
    if expected_host.endswith("-unknown-linux-gnu"):
        lld_driver_directory = root / "bundled rust-lld driver"
        lld_driver_directory.mkdir()
        (lld_driver_directory / "ld.lld").symlink_to(rust_lld)
        lld_label = "bundled rust-lld (ld.lld via native driver)"
        lld_overrides = {
            target_linker_environment: str(driver),
            "CARGO_ENCODED_RUSTFLAGS": (
                f"-Clink-arg=-B{lld_driver_directory}{os.sep}"
                "\x1f-Clink-arg=-fuse-ld=lld"
            ),
        }
        lld_probe_response_argument = driver_response_argument

    configurations = (
        (
            "explicit default driver",
            f"[target.{expected_host}]\nlinker = {json.dumps(str(poison_linker))}\n",
            {
                target_linker_environment: str(driver),
            },
            driver_response_argument,
        ),
        (
            lld_label,
            (f'[build]\nrustflags = ["-C", {json.dumps(f"linker={poison_linker}")}]\n'),
            lld_overrides,
            lld_probe_response_argument,
        ),
    )

    for index, (label, cargo_config, overrides, response_argument) in enumerate(
        configurations
    ):
        workspace = root / f"native-linker-workspace-{index}"
        target = root / f"native-linker-target-{index}"
        response = root / f"native linker response {index}.rsp"
        shutil.copytree(FIXTURE_ROOT, workspace)
        (workspace / ".cargo").mkdir()
        (workspace / ".cargo/config.toml").write_text(
            cargo_config, encoding="utf-8", newline="\n"
        )
        response.write_text(f"{response_argument}\n", encoding="utf-8", newline="\n")
        probe_env = {
            **env,
            **overrides,
            "CARGO_RAIL_COMPAT_LINK_RESPONSE": str(response),
        }
        initialize_git_repository(workspace, probe_env)
        case = ActionCase(
            name=label,
            action="distribution",
            cargo_argv=(
                "cargo",
                "build",
                "--workspace",
                "--release",
                "--locked",
                "--quiet",
            ),
            rail_run_args=("--quiet",),
        )
        assert_expanded_argv(
            cargo_rail, case, target, workspace=workspace, env=probe_env
        )
        direct, direct_outputs = execute_case(
            cargo_rail,
            case,
            target,
            workspace=workspace,
            env=probe_env,
        )
        assert_release_binary(target, workspace, probe_env)
        assert_explicit_bypass_explanation(
            cargo_rail,
            case,
            target,
            ("configured_linker_not_graduated",),
            direct,
            direct_outputs,
            workspace=workspace,
            env=probe_env,
        )


def assert_codegen_backend_probes(
    cargo_rail: Path,
    root: Path,
    *,
    env: dict[str, str],
) -> None:
    workspace = root / "codegen-backend-workspace"
    shutil.copytree(FIXTURE_ROOT, workspace)
    initialize_git_repository(workspace, env)

    success_target = root / "codegen-backend-target"
    success_env = {
        **env,
        "CARGO_ENCODED_RUSTFLAGS": "-Zcodegen-backend=cranelift",
    }
    success_case = ActionCase(
        name="Cranelift codegen backend",
        action="distribution",
        cargo_argv=(
            "cargo",
            "build",
            "--workspace",
            "--release",
            "--locked",
            "--quiet",
        ),
        rail_run_args=("--quiet",),
    )
    assert_expanded_argv(
        cargo_rail,
        success_case,
        success_target,
        workspace=workspace,
        env=success_env,
    )
    direct, direct_outputs = execute_case(
        cargo_rail,
        success_case,
        success_target,
        workspace=workspace,
        env=success_env,
    )
    assert_release_binary(success_target, workspace, success_env)
    assert_explicit_bypass_explanation(
        cargo_rail,
        success_case,
        success_target,
        ("codegen_backend_not_graduated",),
        direct,
        direct_outputs,
        workspace=workspace,
        env=success_env,
    )

    failure_target = root / "unknown-codegen-backend-target"
    failure_env = {
        **env,
        "CARGO_ENCODED_RUSTFLAGS": "-Zcodegen-backend=cargo_rail_missing_backend",
    }
    failure_case = ActionCase(
        name="unknown codegen backend",
        action="build",
        cargo_argv=(
            "cargo",
            "check",
            "--workspace",
            "--locked",
            "--quiet",
        ),
        rail_run_args=("--locked", "--quiet"),
    )
    direct_failure, _ = assert_command_parity(
        failure_case.name,
        direct_argv(failure_case, failure_target),
        (
            (
                "cache-disabled",
                rail_argv(
                    cargo_rail,
                    failure_case,
                    failure_target,
                    no_cache=True,
                    explain=False,
                    dry_run_json=False,
                ),
            ),
            (
                "cache-requested",
                rail_argv(
                    cargo_rail,
                    failure_case,
                    failure_target,
                    no_cache=False,
                    explain=False,
                    dry_run_json=False,
                ),
            ),
        ),
        failure_target,
        workspace=workspace,
        env=failure_env,
        expect_success=False,
    )
    if b"cargo_rail_missing_backend" not in direct_failure.stderr:
        raise CompatibilityError(
            "unknown backend failure did not retain rustc's backend diagnostic"
        )


def assert_command_parity(
    label: str,
    direct_argv: list[str],
    rail_invocations: tuple[tuple[str, list[str]], ...],
    target: Path,
    *,
    workspace: Path,
    env: dict[str, str],
    expect_success: bool = True,
) -> tuple[ProcessResult, tuple[tuple[str, str, int, str], ...]]:
    clean_directory(target)
    direct = run(
        direct_argv,
        cwd=workspace,
        env=env,
        expected_codes=(0,) if expect_success else None,
    )
    if not expect_success and direct.returncode == 0:
        raise CompatibilityError(f"{label} unexpectedly succeeded")
    direct_outputs = output_manifest(target, allow_empty=not expect_success)
    for mode, argv in rail_invocations:
        clean_directory(target)
        result = run(
            argv,
            cwd=workspace,
            env=env,
            expected_codes=(direct.returncode,),
        )
        if result != direct:
            raise CompatibilityError(
                f"{label} {mode} process result differs from direct Cargo\n"
                f"direct={direct!r}\nrail={result!r}"
            )
        outputs = output_manifest(target, allow_empty=not expect_success)
        if outputs != direct_outputs:
            raise CompatibilityError(
                f"{label} {mode} outputs differ from direct Cargo:\n"
                f"{manifest_difference(direct_outputs, outputs)}"
            )
    return direct, direct_outputs


def rustup_program(
    toolchain: str, program: str, workspace: Path, env: dict[str, str]
) -> str:
    result = run(
        ["rustup", "which", "--toolchain", toolchain, program],
        cwd=workspace,
        env=env,
    )
    path = result.stdout.decode("utf-8").strip()
    if not path or not Path(path).is_file():
        raise CompatibilityError(
            f"rustup did not resolve {program} for {toolchain}: {path!r}"
        )
    return path


def create_sysroot_alias(
    sysroot: Path, alias: Path, workspace: Path, env: dict[str, str]
) -> None:
    if os.name == "nt":
        run(
            ["cmd", "/c", "mklink", "/J", str(alias), str(sysroot)],
            cwd=workspace,
            env=env,
        )
    else:
        alias.symlink_to(sysroot, target_is_directory=True)


def assert_toolchain_selection_modes(
    cargo_rail: Path,
    toolchain: str,
    target: Path,
    root: Path,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    direct_check = [
        "cargo",
        "check",
        "--workspace",
        "--locked",
        "--quiet",
        "--target-dir",
        str(target),
    ]

    plus_environment = env.copy()
    plus_environment.pop("RUSTUP_TOOLCHAIN", None)
    plus_environment["PATH"] = (
        str(cargo_rail.parent) + os.pathsep + plus_environment.get("PATH", "")
    )
    assert_command_parity(
        "explicit +toolchain",
        ["cargo", f"+{toolchain}", *direct_check[1:]],
        (
            (
                "cache-disabled",
                [
                    "cargo",
                    f"+{toolchain}",
                    "rail",
                    "run",
                    "--quiet",
                    "--all",
                    "--action",
                    "build",
                    "--no-cache",
                    "--",
                    "--locked",
                    "--quiet",
                    "--target-dir",
                    str(target),
                ],
            ),
            (
                "cache-requested",
                [
                    "cargo",
                    f"+{toolchain}",
                    "rail",
                    "run",
                    "--quiet",
                    "--all",
                    "--action",
                    "build",
                    "--",
                    "--locked",
                    "--quiet",
                    "--target-dir",
                    str(target),
                ],
            ),
        ),
        target,
        workspace=workspace,
        env=plus_environment,
    )

    explicit_environment = env.copy()
    explicit_environment.pop("RUSTUP_TOOLCHAIN", None)
    cargo = rustup_program(toolchain, "cargo", workspace, env)
    explicit_environment.update(
        {
            "CARGO": cargo,
            "RUSTC": rustup_program(toolchain, "rustc", workspace, env),
            "RUSTDOC": rustup_program(toolchain, "rustdoc", workspace, env),
        }
    )
    assert_command_parity(
        "explicit Cargo/rustc/rustdoc",
        [cargo, *direct_check[1:]],
        (
            (
                "cache-disabled",
                [
                    str(cargo_rail),
                    "rail",
                    "run",
                    "--quiet",
                    "--all",
                    "--action",
                    "build",
                    "--no-cache",
                    "--",
                    "--locked",
                    "--quiet",
                    "--target-dir",
                    str(target),
                ],
            ),
            (
                "cache-requested",
                [
                    str(cargo_rail),
                    "rail",
                    "run",
                    "--quiet",
                    "--all",
                    "--action",
                    "build",
                    "--",
                    "--locked",
                    "--quiet",
                    "--target-dir",
                    str(target),
                ],
            ),
        ),
        target,
        workspace=workspace,
        env=explicit_environment,
    )

    sysroot = Path(
        run(["rustc", "--print=sysroot"], cwd=workspace, env=env)
        .stdout.decode("utf-8")
        .strip()
    )
    sysroot_alias = root / "custom sysroot"
    create_sysroot_alias(sysroot, sysroot_alias, workspace, env)
    sysroot_environment = env.copy()
    sysroot_environment["CARGO_ENCODED_RUSTFLAGS"] = f"--sysroot={sysroot_alias}"
    rail_base = [
        str(cargo_rail),
        "rail",
        "run",
        "--quiet",
        "--all",
        "--action",
        "build",
    ]
    rail_arguments = ["--", "--locked", "--quiet", "--target-dir", str(target)]
    direct, direct_outputs = assert_command_parity(
        "custom sysroot",
        direct_check,
        (
            ("cache-disabled", [*rail_base, "--no-cache", *rail_arguments]),
            ("cache-requested", [*rail_base, *rail_arguments]),
        ),
        target,
        workspace=workspace,
        env=sysroot_environment,
    )
    clean_directory(target)
    explained = run(
        [*rail_base, "--explain", *rail_arguments],
        cwd=workspace,
        env=sysroot_environment,
    )
    if explained.returncode != direct.returncode or explained.stderr != direct.stderr:
        raise CompatibilityError("custom sysroot explain changed exit status or stderr")
    explained_outputs = output_manifest(target)
    if explained_outputs != direct_outputs:
        raise CompatibilityError(
            "custom sysroot explain changed outputs:\n"
            f"{manifest_difference(direct_outputs, explained_outputs)}"
        )
    expected = "native compiler cache: bypassed (custom_sysroot_not_graduated)"
    if expected not in explained.stdout.decode("utf-8"):
        raise CompatibilityError(
            f"custom sysroot did not report {expected}:\n{explained.stdout.decode(errors='replace')}"
        )


def assert_incoherent_toolchain_selection(
    cargo_rail: Path,
    primary_toolchain: str,
    alternate_toolchain: str,
    target: Path,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    alternate_cargo = rustup_program(alternate_toolchain, "cargo", workspace, env)
    mixed_environment = env.copy()
    mixed_environment.update(
        {
            "CARGO": alternate_cargo,
            "RUSTC": rustup_program(primary_toolchain, "rustc", workspace, env),
            "RUSTDOC": rustup_program(primary_toolchain, "rustdoc", workspace, env),
        }
    )
    direct_argv = [
        alternate_cargo,
        "check",
        "--workspace",
        "--locked",
        "--quiet",
        "--target-dir",
        str(target),
    ]
    rail_base = [
        str(cargo_rail),
        "rail",
        "run",
        "--quiet",
        "--all",
        "--action",
        "build",
    ]
    rail_arguments = ["--", "--locked", "--quiet", "--target-dir", str(target)]
    direct, direct_outputs = assert_command_parity(
        "incoherent Cargo/rustc/rustdoc selection",
        direct_argv,
        (
            ("cache-disabled", [*rail_base, "--no-cache", *rail_arguments]),
            ("cache-requested", [*rail_base, *rail_arguments]),
        ),
        target,
        workspace=workspace,
        env=mixed_environment,
    )

    clean_directory(target)
    explained = run(
        [*rail_base, "--explain", *rail_arguments],
        cwd=workspace,
        env=mixed_environment,
    )
    if explained.returncode != direct.returncode or explained.stderr != direct.stderr:
        raise CompatibilityError(
            "incoherent toolchain explain changed exit status or stderr"
        )
    explained_outputs = output_manifest(target)
    if explained_outputs != direct_outputs:
        raise CompatibilityError(
            "incoherent toolchain explain changed outputs:\n"
            f"{manifest_difference(direct_outputs, explained_outputs)}"
        )
    expected = "native compiler cache: bypassed (native_cache_toolchain_incoherent)"
    if expected not in explained.stdout.decode("utf-8"):
        raise CompatibilityError(
            f"incoherent toolchain did not report {expected}:\n"
            f"{explained.stdout.decode(errors='replace')}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cargo-rail", type=Path, required=True)
    parser.add_argument("--toolchain", required=True)
    parser.add_argument("--expected-rust-release")
    parser.add_argument("--expected-host", required=True)
    parser.add_argument("--expected-cache-state", required=True)
    parser.add_argument("--selection-probes", action="store_true")
    parser.add_argument("--incoherent-toolchain")
    parser.add_argument("--skip-cross-target-corpus", action="store_true")
    parser.add_argument("--cross-target-mutation-probes", action="store_true")
    parser.add_argument("--custom-target-json-probe", action="store_true")
    parser.add_argument("--linker-probes", action="store_true")
    parser.add_argument("--codegen-backend-probes", action="store_true")
    parser.add_argument("--direct-repeatability-probe", action="store_true")
    parser.add_argument("--temporary-root", type=Path)
    args = parser.parse_args()

    cargo_rail = args.cargo_rail.resolve()
    if not cargo_rail.is_file():
        print(
            f"compatibility: cargo-rail executable does not exist: {cargo_rail}",
            file=sys.stderr,
        )
        return 2
    if args.temporary_root is not None and not args.temporary_root.is_dir():
        print(
            f"compatibility: temporary root is not a directory: {args.temporary_root}",
            file=sys.stderr,
        )
        return 2
    if args.skip_cross_target_corpus and args.cross_target_mutation_probes:
        print(
            "compatibility: mutation probes require the cross-target corpus",
            file=sys.stderr,
        )
        return 2
    if args.codegen_backend_probes and not args.toolchain.startswith("nightly-"):
        print(
            "compatibility: codegen backend probes require a dated nightly toolchain",
            file=sys.stderr,
        )
        return 2

    try:
        with tempfile.TemporaryDirectory(
            prefix="cargo-rail-compatibility-",
            dir=args.temporary_root,
        ) as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            target = root / "target"
            environment = selected_environment(
                args.toolchain, root / "cargo-home", root / "cache"
            )
            if args.expected_host.endswith("-pc-windows-msvc"):
                # MSVC's default debug-link products vary across otherwise identical
                # clean builds. Use link.exe's native reproducible mode so the PE byte
                # comparison remains a parity oracle without changing rustc selection.
                environment["LINK"] = "/Brepro"
                environment["SOURCE_DATE_EPOCH"] = "1"
            initialize_workspace(workspace, environment)
            assert_selected_toolchain(
                args.toolchain,
                args.expected_rust_release,
                args.expected_host,
                cwd=workspace,
                env=environment,
            )
            assert_plan(cargo_rail, workspace, environment)
            expected_cache_state = resolve_expected_cache_state(
                cargo_rail,
                args.expected_cache_state,
                args.expected_host,
                workspace=workspace,
                env=environment,
            )

            cases = (
                ActionCase(
                    "check",
                    "build",
                    ("cargo", "check", "--workspace", "--locked", "--quiet"),
                    rail_run_args=("--locked", "--quiet"),
                ),
                ActionCase(
                    "build",
                    "distribution",
                    (
                        "cargo",
                        "build",
                        "--workspace",
                        "--release",
                        "--locked",
                        "--quiet",
                    ),
                    rail_run_args=("--quiet",),
                ),
                ActionCase(
                    "test --no-run",
                    "test",
                    ("cargo", "test", "-p", PACKAGE, "--no-run", "--locked", "--quiet"),
                    ("--test-runner", "cargo"),
                    rail_cargo_test_args=("--no-run", "--locked", "--quiet"),
                ),
            )
            for case in cases:
                assert_expanded_argv(
                    cargo_rail, case, target, workspace=workspace, env=environment
                )
                direct, direct_outputs = execute_case(
                    cargo_rail,
                    case,
                    target,
                    workspace=workspace,
                    env=environment,
                    verify_direct_repeatability=(
                        args.direct_repeatability_probe and case.name == "check"
                    ),
                )
                if case.action in {"build", "distribution"}:
                    assert_cache_explanation(
                        cargo_rail,
                        case,
                        target,
                        expected_cache_state,
                        direct,
                        direct_outputs,
                        workspace=workspace,
                        env=environment,
                    )
                if case.action == "distribution":
                    assert_release_binary(target, workspace, environment)
            if not args.skip_cross_target_corpus:
                assert_cross_target_corpus(
                    cargo_rail,
                    root,
                    action_cache_state=expected_cache_state,
                    env=environment,
                )
            if args.cross_target_mutation_probes:
                assert_cross_target_mutations(
                    cargo_rail,
                    root,
                    args.expected_host,
                    action_cache_state=expected_cache_state,
                    env=environment,
                )
            if args.custom_target_json_probe:
                assert_custom_target_json(cargo_rail, root, env=environment)
            if args.linker_probes:
                assert_native_linker_probes(
                    cargo_rail,
                    root,
                    args.expected_host,
                    env=environment,
                )
            if args.codegen_backend_probes:
                assert_codegen_backend_probes(
                    cargo_rail,
                    root,
                    env=environment,
                )
            if args.selection_probes:
                assert_toolchain_selection_modes(
                    cargo_rail,
                    args.toolchain,
                    target,
                    root,
                    workspace=workspace,
                    env=environment,
                )
            if args.incoherent_toolchain:
                assert_incoherent_toolchain_selection(
                    cargo_rail,
                    args.toolchain,
                    args.incoherent_toolchain,
                    target,
                    workspace=workspace,
                    env=environment,
                )
        print(
            f"compatibility: {args.expected_host} with {args.toolchain} passed "
            "plan/check/build/test and requested linker/backend cache parity"
        )
        return 0
    except (CompatibilityError, OSError) as error:
        print(f"compatibility: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
