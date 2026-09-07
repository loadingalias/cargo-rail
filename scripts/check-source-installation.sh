#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ ${1:-} != --prepare || $# != 2 ]]; then
  echo "usage: $0 --prepare <component-dir>" >&2
  exit 2
fi

python3 - "$2" <<'PYTHON'
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

destination = Path(sys.argv[1]).resolve()
destination.mkdir(parents=True, exist_ok=True)
environment = {name: value for name, value in os.environ.items()
               if not name.startswith('CARGO_RAIL_FACT_DRIVER_')}
for name in ('RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER',
             'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'):
    environment[name] = ''
metadata = json.loads(subprocess.check_output(
    ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline'],
    env=environment, text=True))
target = Path(metadata['target_directory']) / 'source-installation-test'
verbose = subprocess.check_output(['rustc', '-vV'], env=environment, text=True)
host = next(line.removeprefix('host: ') for line in verbose.splitlines() if line.startswith('host: '))
subprocess.run(['cargo', 'build', '--bin', 'cargo-rail', '--all-features', '--locked',
                '--target-dir', str(target), '--target', host], env=environment, check=True)
binary = target / host / 'debug' / ('cargo-rail.exe' if os.name == 'nt' else 'cargo-rail')
(destination / 'source-installation-authority.env').write_text(
    'export CARGO_RAIL_TEST_SOURCE_BINARY=' + shlex.quote(str(binary)) + '\n')
PYTHON
