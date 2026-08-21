#!/usr/bin/env bash

# Git Bash puts its POSIX `link.exe` before MSVC's linker. Resolve the captured
# Visual Studio toolchain explicitly in PATH without injecting linker arguments
# into Cargo's build or cache identity.

if [ "$#" -ne 1 ]; then
  echo "usage: windows-recipe-shell.sh <command>" >&2
  exit 2
fi

msvc_bin=""
while IFS= read -r candidate; do
  candidate=${candidate%$'\r'}
  candidate=$(cygpath -u "$candidate")
  case "$candidate" in
    */VC/Tools/MSVC/*/bin/Host*/*/link.exe)
      msvc_bin=${candidate%/link.exe}
      break
      ;;
  esac
done < <(where.exe link.exe 2>/dev/null || true)

if [ -z "$msvc_bin" ]; then
  echo "MSVC link.exe is unavailable in the captured Windows tool environment" >&2
  exit 2
fi

export PATH="$msvc_bin:$PATH"
eval "$1"
