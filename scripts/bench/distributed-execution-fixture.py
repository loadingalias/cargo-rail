#!/usr/bin/env python3
"""Generate an eligible distributed-execution library fixture.

Each source file has codegen cost that scales with the requested function
count. Generic bodies keep the measured work in real monomorphization instead
of source size alone.

`--crates` emits an even-sized workspace in two dependency-bearing waves. The
first half are mutually independent producers; every crate in the second half
uses one exact producer. Each wave therefore exposes several ready actions,
while the second proves real `.rmeta`/`.rlib` transfer instead of measuring only
dependency-free leaves. On equal hardware a remote compile costs about what the
same local compile costs, so a single action can only measure transfer, lease,
and admission overhead rather than added critical-path capacity.
"""

from __future__ import annotations

import argparse
from pathlib import Path

MAX_FUNCTIONS = 99999
MAX_CRATES = 512


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("--functions", type=int, required=True)
    value.add_argument("--crates", type=int, default=0, help="emit an even-sized two-wave workspace")
    value.add_argument("--output", required=True, help="source file, or workspace root when --crates is used")
    return value


def source(functions: int, salt: int) -> str:
    body = ["#![forbid(unsafe_code)]\n"]
    for index in range(1, functions + 1):
        seed = index + salt
        body.append(
            f"pub fn value_{index}<T: Copy + Into<u64>>(input: T) -> u64 {{\n"
            f"    let mut acc = input.into() ^ {seed};\n"
            f"    for step in 0..{index % 23 + 5}u32 {{\n"
            "        acc = acc.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(step % 63 + 1) ^ (acc >> 29);\n"
            f"        acc = acc.wrapping_add(u64::from(step)).wrapping_mul({seed} + 1);\n"
            "    }\n"
            "    acc\n"
            "}\n"
            f"pub fn call_{index}(first: u8, second: u32) -> u64 {{"
            f" value_{index}(first) ^ value_{index}(second) }}\n"
        )
    return "".join(body)


def member_manifest(name: str, dependency: str | None) -> str:
    manifest = f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2024"\n\n[lib]\npath = "src/lib.rs"\n'
    if dependency is not None:
        manifest += f'\n[dependencies]\n{dependency} = {{ path = "../{dependency}" }}\n'
    return manifest


def write_workspace(root: Path, functions: int, crates: int) -> None:
    if root.exists():
        if not root.is_dir() or any(root.iterdir()):
            raise ValueError(f"workspace output is not an empty directory: {root}")
    else:
        root.mkdir(parents=True)
    members = [f"distributed-member-{index:03d}" for index in range(1, crates + 1)]
    listed = ", ".join(f'"{member}"' for member in members)
    (root / "Cargo.toml").write_text(
        f"[workspace]\nmembers = [{listed}]\nresolver = \"3\"\n\n"
        "[profile.release]\nincremental = false\n",
        encoding="utf-8",
    )
    producers = crates // 2
    for index, member in enumerate(members, start=1):
        package = root / member
        (package / "src").mkdir(parents=True, exist_ok=True)
        dependency = members[index - producers - 1] if index > producers else None
        (package / "Cargo.toml").write_text(member_manifest(member, dependency), encoding="utf-8")
        # Salting keeps every member a distinct compiler action, so no member can
        # be served from another member's cached result.
        contents = source(functions, index * 1_000)
        if dependency is not None:
            dependency_crate = dependency.replace("-", "_")
            contents += (
                f"pub fn dependency_anchor(value: u8) -> u64 {{\n"
                f"    {dependency_crate}::call_1(value, u32::from(value))\n"
                "}\n"
            )
        (package / "src" / "lib.rs").write_text(contents, encoding="utf-8")


def main() -> int:
    arguments = parser().parse_args()
    if not 1 <= arguments.functions <= MAX_FUNCTIONS:
        parser().error(f"unsupported fixture size: {arguments.functions}")
    if arguments.crates and (not 2 <= arguments.crates <= MAX_CRATES or arguments.crates % 2 != 0):
        parser().error(f"unsupported crate count: {arguments.crates}")
    output = Path(arguments.output)
    if arguments.crates:
        try:
            write_workspace(output, arguments.functions, arguments.crates)
        except ValueError as error:
            parser().error(str(error))
    else:
        output.write_text(source(arguments.functions, 0), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
