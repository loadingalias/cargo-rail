#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
[[ $# == 1 ]] || { echo "usage: $0 <component-dir>" >&2; exit 2; }

python3 - "$1" <<'PYTHON'
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tomllib

policy = tomllib.loads(Path('.config/cranelift-toolchain.toml').read_text())['toolchain']
toolchain = policy['channel']
environment = os.environ.copy()
environment['RUSTUP_AUTO_INSTALL'] = '0'
installed = subprocess.run(
    ['rustup', 'component', 'list', '--toolchain', toolchain, '--installed'],
    env=environment, capture_output=True, text=True)
components = installed.stdout.splitlines() if installed.returncode == 0 else []
if not all(any(line == name or line.startswith(name + '-') for line in components)
           for name in policy['components']):
    subprocess.run(['rustup', 'toolchain', 'install', toolchain, '--profile', policy['profile'],
                    '--component', ','.join(policy['components'])], check=True, env=environment)
verbose = subprocess.check_output(['rustup', 'run', toolchain, 'rustc', '-vV'],
                                  env=environment, text=True)
host = next(line.removeprefix('host: ') for line in verbose.splitlines() if line.startswith('host: '))
sysroot = Path(subprocess.check_output(['rustup', 'run', toolchain, 'rustc', '--print', 'sysroot'],
                                     env=environment, text=True).strip())
backend = list((sysroot / 'lib/rustlib' / host / 'codegen-backends').glob('*rustc_codegen_cranelift*'))
development = list((sysroot / 'lib/rustlib' / host / 'lib').glob('librustc_hir-*'))
if len(backend) != 1 or not development:
    raise SystemExit('Cranelift requires exactly one backend and matched rustc-dev files')
destination = Path(sys.argv[1]).resolve()
destination.mkdir(parents=True, exist_ok=True)
(destination / 'cranelift-toolchain.env').write_text(
    'export CARGO_RAIL_TEST_CRANELIFT_TOOLCHAIN=' + shlex.quote(toolchain) + '\n')
print(f'Prepared Cranelift test toolchain: {toolchain} ({host})')
PYTHON
