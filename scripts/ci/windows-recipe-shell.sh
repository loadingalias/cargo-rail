#!/usr/bin/env bash

# Git Bash puts its POSIX `link.exe` before MSVC's linker. Resolve the native
# Visual Studio linker explicitly in PATH without injecting linker arguments
# into Cargo's build or cache identity.

if [ "$#" -ne 1 ]; then
  echo "usage: windows-recipe-shell.sh <command>" >&2
  exit 2
fi

rust_host=$(rustc -vV | sed -n 's/^host: //p')
case "$rust_host" in
  aarch64-pc-windows-msvc)
    target_arch=arm64
    ;;
  x86_64-pc-windows-msvc)
    target_arch=x64
    ;;
  *)
    echo "unsupported native Windows Rust host: ${rust_host:-unknown}" >&2
    exit 2
    ;;
esac

msvc_bin=""
find_msvc_bin() {
  local candidate normalized
  while IFS= read -r candidate; do
    candidate=${candidate%$'\r'}
    candidate=$(cygpath -u "$candidate")
    normalized=$(printf '%s' "$candidate" | tr '[:upper:]' '[:lower:]')
    case "$normalized" in
      */vc/tools/msvc/*/bin/host*/"$target_arch"/link.exe)
        msvc_bin=${candidate%/*}
        return
        ;;
    esac
  done
}

find_msvc_bin < <(where.exe link.exe 2>/dev/null || true)

if [ -z "$msvc_bin" ]; then
  program_files_x86=$(printenv 'ProgramFiles(x86)' 2>/dev/null || true)
  vswhere=$(cygpath -u "$program_files_x86/Microsoft Visual Studio/Installer/vswhere.exe")
  if [ -x "$vswhere" ]; then
    find_msvc_bin < <(
      "$vswhere" -latest -products '*' -find 'VC\Tools\MSVC\**\bin\Host*\*\link.exe' 2>/dev/null || true
    )
  fi
fi

if [ -z "$msvc_bin" ]; then
  echo "MSVC link.exe is unavailable in the installed Windows tool environment" >&2
  exit 2
fi

export PATH="$msvc_bin:$PATH"
eval "$1"
