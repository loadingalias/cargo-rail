#!/usr/bin/env python3
"""Exercise the installed native toolchain before repository checks start."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

import catalog


def run(*arguments, cwd=None):
    print('+ ' + ' '.join(map(str, arguments)), flush=True)
    result = subprocess.run(arguments, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, end='', flush=True)
    result.check_returncode()
    return result.stdout


def verify_version(name, version, command):
    if not re.search(r'(?<![\d.])' + re.escape(version) + r'(?![\d.])', run(*command)):
        raise ValueError(f'{name} does not match its pinned version')


def verify(platform):
    data = catalog.read()
    native = data[platform]
    version = run('rustc', '-vV')
    if f'host: {native["rust-host"]}\n' not in version or f'release: {catalog.rust_channel()}\n' not in version:
        raise ValueError('active Rust compiler does not match the catalog')
    verify_version('cargo', catalog.rust_channel(), ['cargo', '--version'])
    installed = run('rustup', 'component', 'list', '--installed').splitlines()
    for component in native['components']:
        if not any(line.startswith(component + '-') for line in installed):
            raise ValueError(f'missing Rust component: {component}')
    pinned_commands = [('rustup', ['rustup', '--version'])]
    for name, command in (
        ('cargo-binstall', ['cargo', 'binstall', '-V']),
        ('cmake', ['cmake', '--version']),
        ('llvm', ['clang', '--version']),
        ('git', ['git', '--version']),
        ('jq', ['jq', '--version']),
        ('python', [sys.executable, '--version']),
        ('powershell', ['pwsh', '-NoProfile', '-NonInteractive', '-Command', '$PSVersionTable.PSVersion.ToString()']),
    ):
        if name in native['assets']:
            pinned_commands.append((name, command))
    for name, command in pinned_commands:
        verify_version(name, data['versions'][name], command)
    tools = list(native['cargo'])
    if not tools:
        tools = ['cargo-nextest', 'just']
    for tool in tools:
        command = ['cargo', tool.removeprefix('cargo-')] if tool.startswith('cargo-') else [{'ripgrep': 'rg'}.get(tool, tool)]
        verify_version(tool, data['cargo'][tool], [*command, '--version'])
    for tool in ('git', 'bash'):
        run(tool, '--version')
    with tempfile.TemporaryDirectory(prefix='rail-tooling-') as temporary:
        root = Path(temporary)
        suffix = '.exe' if os.name == 'nt' else ''
        (root / 'smoke.rs').write_text('fn main() { println!("native-rust-ok"); }\n')
        run('rustc', 'smoke.rs', '-o', 'rust-smoke' + suffix, cwd=root)
        if run(str(root / ('rust-smoke' + suffix))).strip() != 'native-rust-ok':
            raise ValueError('native Rust executable produced incorrect output')
        (root / 'smoke.c').write_text('#include <stdio.h>\nint main(void) { puts("native-c-ok"); return 0; }\n')
        if os.name == 'nt':
            linker = Path(shutil.which('link.exe') or '').resolve()
            if not linker.is_relative_to(Path(os.environ['VCToolsInstallDir']).resolve()):
                raise ValueError(f'MSVC linker is shadowed: {linker}')
            run('cl', '/nologo', 'smoke.c', '/Fe:c-smoke.exe', cwd=root)
        else:
            run('cc', '--version')
            run('cc', 'smoke.c', '-o', 'c-smoke', cwd=root)
        if run(str(root / ('c-smoke' + suffix))).strip() != 'native-c-ok':
            raise ValueError('native C executable produced incorrect output')
        if 'cmake' in native['assets']:
            (root / 'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.20)\nproject(smoke C)\nadd_executable(cmake-smoke smoke.c)\n')
            run('cmake', '-S', '.', '-B', 'build', '-G', 'NMake Makefiles' if os.name == 'nt' else 'Unix Makefiles', cwd=root)
            run('cmake', '--build', 'build', cwd=root)
            run(str(root / 'build' / ('cmake-smoke' + suffix)))
        if 'llvm' in native['assets']:
            run('clang', 'smoke.c', '-o', 'clang-smoke' + suffix, cwd=root)
            run(str(root / ('clang-smoke' + suffix)))
    print(f'Installed {platform} toolchain passed native compilation and execution checks.')


if __name__ == '__main__':
    verify(sys.argv[1])
