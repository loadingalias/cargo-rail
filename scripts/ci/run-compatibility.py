#!/usr/bin/env python3
"""Qualify planner scope, direct Cargo execution, and transparent reuse."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
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
PLAN_CONTRACT_VERSION = 8
SCRUBBED_ENVIRONMENT = (
    "CARGO",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
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
class CargoCase:
    name: str
    surface: str
    arguments: tuple[str, ...]


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
    expected_codes: tuple[int, ...] = (0,),
) -> ProcessResult:
    completed = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, check=False)
    result = ProcessResult(completed.returncode, completed.stdout, completed.stderr)
    if result.returncode not in expected_codes:
        raise CompatibilityError(
            f"command exited {result.returncode}, expected {expected_codes}: {display_argv(argv)}\n"
            f"stdout:\n{result.stdout.decode(errors='replace')}\n"
            f"stderr:\n{result.stderr.decode(errors='replace')}"
        )
    return result


def selected_environment(toolchain: str, cargo_home: Path, cache: Path) -> dict[str, str]:
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
    versions = {
        program: first_line(run([program, "--version"], cwd=cwd, env=env))
        for program in ("cargo", "rustc", "rustdoc")
    }
    rustc_verbose = run(["rustc", "--version", "--verbose"], cwd=cwd, env=env).stdout.decode("utf-8")
    host = next(
        (line.removeprefix("host:").strip() for line in rustc_verbose.splitlines() if line.startswith("host:")),
        "",
    )
    if host != expected_host:
        raise CompatibilityError(f"selected rustc host is {host!r}, expected {expected_host!r}")
    if expected_release is not None:
        for program, value in versions.items():
            if not value.startswith(f"{program} {expected_release} "):
                raise CompatibilityError(f"{program} did not resolve through Rust {expected_release}: {value}")
    elif toolchain.startswith("nightly-"):
        if "nightly" not in versions["rustc"]:
            raise CompatibilityError(f"rustc did not resolve through {toolchain}: {versions['rustc']}")
    elif toolchain not in versions["rustc"]:
        raise CompatibilityError(f"rustc did not resolve through {toolchain}: {versions['rustc']}")


def initialize_workspace(destination: Path, env: dict[str, str]) -> None:
    shutil.copytree(FIXTURE_ROOT, destination)
    git_environment = env | {
        "GIT_AUTHOR_EMAIL": "compatibility@example.invalid",
        "GIT_AUTHOR_NAME": "cargo-rail compatibility",
        "GIT_COMMITTER_EMAIL": "compatibility@example.invalid",
        "GIT_COMMITTER_NAME": "cargo-rail compatibility",
    }
    run(["git", "init", "--quiet"], cwd=destination, env=git_environment)
    run(["git", "config", "core.autocrlf", "false"], cwd=destination, env=git_environment)
    run(["git", "add", "--all"], cwd=destination, env=git_environment)
    run(["git", "commit", "--quiet", "-m", "compatibility baseline"], cwd=destination, env=git_environment)
    with (destination / "src/lib.rs").open("a", encoding="utf-8", newline="\n") as source:
        source.write("\n// Give the planner one deterministic changed source.\n")
    run(["git", "add", "src/lib.rs"], cwd=destination, env=git_environment)
    run(["git", "commit", "--quiet", "-m", "exercise planner"], cwd=destination, env=git_environment)


def planner_scopes(cargo_rail: Path, workspace: Path, env: dict[str, str]) -> dict[str, tuple[str, ...]]:
    result = run(
        [str(cargo_rail), "rail", "plan", "--since", "HEAD^", "--json"],
        cwd=workspace,
        env=env,
    )
    try:
        plan = json.loads(result.stdout)
        if plan["plan_contract_version"] != PLAN_CONTRACT_VERSION:
            raise CompatibilityError("planner contract version changed")
        scopes = {}
        for surface in ("build", "test"):
            decision = plan["work"][f"cargo.{surface}"]
            selection = decision["scope"]["selection"]
            arguments = selection["cargo_args"]
            if decision["state"] != "required" or decision["scope"]["kind"] != "cargo":
                raise CompatibilityError(f"planner did not require cargo.{surface}")
            if not isinstance(arguments, list):
                raise CompatibilityError(f"planner omitted cargo.{surface} arguments")
            if not all(isinstance(argument, str) and argument for argument in arguments):
                raise CompatibilityError(f"planner emitted an invalid {surface} Cargo argument")
            scopes[surface] = tuple(arguments)
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(f"planner stdout is not a valid typed scope: {error}") from error
    return scopes


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(128 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def artifact_digest(path: Path, relative: str) -> str:
    if relative == ".rustc_info.json":
        try:
            value = json.loads(path.read_bytes())
        except (OSError, json.JSONDecodeError) as error:
            raise CompatibilityError(f"Cargo rustc-info cache is invalid JSON: {error}") from error
        canonical = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        return hashlib.sha256(canonical).hexdigest()
    return sha256(path)


def output_manifest(root: Path) -> tuple[tuple[str, str, int, str], ...]:
    if not root.is_dir():
        raise CompatibilityError(f"Cargo produced no target directory at {root}")
    entries = []
    for path in sorted(root.rglob("*"), key=lambda candidate: candidate.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        mode = stat.S_IMODE(metadata.st_mode)
        if path.is_symlink():
            entries.append((relative, "symlink", mode, os.readlink(path)))
        elif path.is_dir():
            entries.append((relative, "directory", mode, ""))
        elif path.is_file():
            if os.name == "nt" and path.suffix.casefold() == ".pdb":
                digest = "msvc-pdb-nondeterministic"
            else:
                digest = artifact_digest(path, relative)
            entries.append((relative, "file", mode, digest))
        else:
            raise CompatibilityError(f"unsupported output kind at {path}")
    if not entries:
        raise CompatibilityError(f"Cargo produced an empty target directory at {root}")
    return tuple(entries)


def manifest_difference(
    expected: tuple[tuple[str, str, int, str], ...],
    actual: tuple[tuple[str, str, int, str], ...],
) -> str:
    expected_by_path = {entry[0]: entry[1:] for entry in expected}
    actual_by_path = {entry[0]: entry[1:] for entry in actual}
    differences = []
    for path in sorted(expected_by_path.keys() | actual_by_path.keys()):
        if expected_by_path.get(path) != actual_by_path.get(path):
            differences.append(
                f"{path}: expected {expected_by_path.get(path)!r}, actual {actual_by_path.get(path)!r}"
            )
    if len(differences) > 20:
        differences = [*differences[:20], f"... {len(differences) - 20} more differences"]
    return "\n".join(differences)


def clean_directory(path: Path) -> None:
    if not path.exists():
        return

    def make_writable_and_retry(function: Any, value: str, _error: Any) -> None:
        os.chmod(value, stat.S_IWRITE)
        function(value)

    shutil.rmtree(path, onerror=make_writable_and_retry)


def cargo_argv(case: CargoCase, scope: tuple[str, ...], target: Path) -> list[str]:
    return ["cargo", *case.arguments, *scope, "--target-dir", str(target)]


def assert_direct_cargo(
    cases: tuple[CargoCase, ...],
    scopes: dict[str, tuple[str, ...]],
    target: Path,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    for case in cases:
        case_target = target / case.name.split()[0]
        clean_directory(case_target)
        argv = cargo_argv(case, scopes[case.surface], case_target)
        first = run(argv, cwd=workspace, env=env)
        first_outputs = output_manifest(case_target)
        if case.name == "check":
            clean_directory(case_target)
            second = run(argv, cwd=workspace, env=env)
            second_outputs = output_manifest(case_target)
            if second != first or second_outputs != first_outputs:
                raise CompatibilityError(
                    "repeated direct Cargo check changed process output or artifacts\n"
                    f"first={first!r}\nsecond={second!r}\n"
                    f"{manifest_difference(first_outputs, second_outputs)}"
                )


def assert_native_cache_identity(cargo_rail: Path, expected_host: str, workspace: Path, env: dict[str, str]) -> None:
    result = run(
        [str(cargo_rail), "rail", "doctor", "native-cache", "--format", "json"],
        cwd=workspace,
        env=env,
    )
    try:
        report = json.loads(result.stdout)
        capability = report["capability"]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(f"native-cache identity report is invalid: {error}") from error
    if (
        report.get("result") != "success"
        or capability.get("host_target") != expected_host
        or capability.get("cache_class") != "exact_rustc_result"
        or capability.get("transported_work_boundary")
        != "moved_root_compiler_work_product_validation_unavailable"
        or re.fullmatch(r"sha256:[0-9a-f]{64}", capability.get("identity", "")) is None
    ):
        raise CompatibilityError("native-cache doctor did not report an exact compiler identity")


def install_transparent_cache(cargo_rail: Path, workspace: Path, env: dict[str, str]) -> None:
    run(
        [
            str(cargo_rail),
            "rail",
            "cache",
            "setup",
            "--local-dir",
            env["CARGO_RAIL_CACHE_DIR"],
            "--max-size",
            "1GiB",
            "--quiet",
        ],
        cwd=workspace,
        env=env,
    )


def assert_cache_was_exercised(cargo_rail: Path, workspace: Path, env: dict[str, str]) -> None:
    result = run(
        [str(cargo_rail), "rail", "cache", "status", "--scope", "local", "--format", "json"],
        cwd=workspace,
        env=env,
    )
    try:
        usage = json.loads(result.stdout)["status"]["installation"]["usage"]
        hits = usage["hits"]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(f"cache usage report is invalid: {error}") from error
    if hits <= 0:
        raise CompatibilityError(f"direct Cargo did not restore a transparent compiler result: {usage!r}")


def cross_target_cases() -> tuple[CrossTargetCase, ...]:
    try:
        entries = json.loads(MANIFEST_PATH.read_bytes())["cross_target_corpus"]
        cases = tuple(
            CrossTargetCase(entry["target"], REPOSITORY_ROOT / entry["fixture"], entry["artifact"])
            for entry in entries
        )
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise CompatibilityError(f"cross-target manifest is invalid: {error}") from error
    if not cases:
        raise CompatibilityError("cross-target manifest is empty")
    return cases


def assert_cross_target_corpus(root: Path, env: dict[str, str], mutate: bool) -> None:
    for case in cross_target_cases():
        workspace = root / "cross-target-workspaces" / case.target
        target = root / "cross-target-artifacts" / case.target
        shutil.copytree(case.fixture, workspace)
        argv = [
            "cargo",
            "build",
            "--manifest-path",
            str(workspace / "Cargo.toml"),
            "--target",
            case.target,
            "--release",
            "--locked",
            "--quiet",
            "--target-dir",
            str(target),
        ]
        run(argv, cwd=workspace, env=env)
        release = target / case.target / "release"
        pattern = "*.wasm" if case.artifact == "wasm" else "*.rlib"
        artifacts = sorted(release.glob(pattern))
        if len(artifacts) != 1:
            raise CompatibilityError(f"{case.target} produced {len(artifacts)} {case.artifact} artifacts")
        if case.artifact == "wasm" and artifacts[0].read_bytes()[:4] != b"\0asm":
            raise CompatibilityError(f"{case.target} output is not WebAssembly")
        if mutate:
            source = next(iter(sorted(workspace.rglob("*.rs"))), None)
            if source is None:
                raise CompatibilityError(f"{case.target} fixture has no Rust source")
            with source.open("a", encoding="utf-8", newline="\n") as stream:
                stream.write("\n// compatibility mutation\n")
            run(argv, cwd=workspace, env=env)


def rustup_program(toolchain: str, program: str, workspace: Path, env: dict[str, str]) -> str:
    path = run(
        ["rustup", "which", "--toolchain", toolchain, program],
        cwd=workspace,
        env=env,
    ).stdout.decode("utf-8").strip()
    if not path or not Path(path).is_file():
        raise CompatibilityError(f"rustup did not resolve {program} for {toolchain}")
    return path


def assert_toolchain_selection(toolchain: str, workspace: Path, target: Path, env: dict[str, str]) -> None:
    plus_env = env.copy()
    plus_env.pop("RUSTUP_TOOLCHAIN", None)
    run(
        ["cargo", f"+{toolchain}", "check", "--workspace", "--locked", "--quiet", "--target-dir", str(target / "plus")],
        cwd=workspace,
        env=plus_env,
    )
    explicit_env = plus_env | {
        "CARGO": rustup_program(toolchain, "cargo", workspace, env),
        "RUSTC": rustup_program(toolchain, "rustc", workspace, env),
        "RUSTDOC": rustup_program(toolchain, "rustdoc", workspace, env),
    }
    run(
        [explicit_env["CARGO"], "check", "--workspace", "--locked", "--quiet", "--target-dir", str(target / "explicit")],
        cwd=workspace,
        env=explicit_env,
    )


def assert_release_binary(target: Path, workspace: Path, env: dict[str, str]) -> None:
    executable = target / "release" / f"{PACKAGE}{'.exe' if os.name == 'nt' else ''}"
    result = run([str(executable)], cwd=workspace, env=env)
    if result.stdout != b"cargo-rail compatibility: 42\n" or result.stderr:
        raise CompatibilityError(f"release fixture output changed: {result!r}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cargo-rail", type=Path, required=True)
    parser.add_argument("--toolchain", required=True)
    parser.add_argument("--expected-rust-release")
    parser.add_argument("--expected-host", required=True)
    parser.add_argument("--selection-probes", action="store_true")
    parser.add_argument("--cross-target-mutation-probes", action="store_true")
    parser.add_argument("--skip-cross-target-corpus", action="store_true")
    parser.add_argument("--temporary-root", type=Path)
    args = parser.parse_args()

    cargo_rail = args.cargo_rail.resolve()
    if not cargo_rail.is_file():
        print(f"compatibility: cargo-rail executable does not exist: {cargo_rail}", file=sys.stderr)
        return 2
    if args.temporary_root is not None and not args.temporary_root.is_dir():
        print(f"compatibility: temporary root is not a directory: {args.temporary_root}", file=sys.stderr)
        return 2
    if args.skip_cross_target_corpus and args.cross_target_mutation_probes:
        print("compatibility: mutation probes require the cross-target corpus", file=sys.stderr)
        return 2

    try:
        with tempfile.TemporaryDirectory(prefix="cargo-rail-compatibility-", dir=args.temporary_root) as temporary:
            root = Path(temporary)
            workspace = root / "workspace"
            target = workspace / "target/compatibility"
            environment = selected_environment(args.toolchain, root / "cargo-home", root / "cache")
            if args.expected_host.endswith("-pc-windows-msvc"):
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
            scopes = planner_scopes(cargo_rail, workspace, environment)
            assert_native_cache_identity(cargo_rail, args.expected_host, workspace, environment)
            install_transparent_cache(cargo_rail, workspace, environment)
            cases = (
                CargoCase("check", "build", ("check", "--locked", "--quiet")),
                CargoCase("build", "build", ("build", "--release", "--locked", "--quiet")),
                CargoCase("test --no-run", "test", ("test", "--no-run", "--locked", "--quiet")),
            )
            assert_direct_cargo(
                cases,
                scopes,
                target,
                workspace=workspace,
                env=environment,
            )
            assert_release_binary(target / "build", workspace, environment)
            assert_cache_was_exercised(cargo_rail, workspace, environment)
            if not args.skip_cross_target_corpus:
                assert_cross_target_corpus(root, environment, args.cross_target_mutation_probes)
            if args.selection_probes:
                assert_toolchain_selection(args.toolchain, workspace, target, environment)
        print(
            f"compatibility: {args.expected_host} with {args.toolchain} passed "
            "planner-scoped check/build/test and transparent-cache qualification"
        )
        return 0
    except (CompatibilityError, OSError) as error:
        print(f"compatibility: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
