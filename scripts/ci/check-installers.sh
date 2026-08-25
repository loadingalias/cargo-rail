#!/usr/bin/env bash
set -euo pipefail

sh -n scripts/install.sh
scripts/ci/test-install.sh

for placeholder in '@CARGO_RAIL_VERSION@'; do
  for installer in scripts/install.sh scripts/install.ps1; do
    count="$(grep -Foc "$placeholder" "$installer")"
    if [ "$count" -ne 1 ]; then
      echo "$installer must contain exactly one $placeholder placeholder" >&2
      exit 1
    fi
  done
done

if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoLogo -NoProfile -Command '
    $tokens = $null
    $errors = $null
    foreach ($file in @("scripts/install.ps1", "scripts/ci/test-install.ps1")) {
      [System.Management.Automation.Language.Parser]::ParseFile(
        (Resolve-Path $file),
        [ref]$tokens,
        [ref]$errors
      ) | Out-Null
      if ($errors.Count -gt 0) {
        $errors | ForEach-Object { [Console]::Error.WriteLine($_) }
        exit 1
      }
    }
  '
fi
