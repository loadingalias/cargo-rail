#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "$0")/.." && pwd -P)"
readonly repository_root
readonly manifest="$repository_root/tools/compiler-fact-driver/Cargo.toml"

[[ "$#" -gt 0 ]] || {
  echo "usage: $0 <command> [argument ...]" >&2
  exit 2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

env RUSTC_BOOTSTRAP=cargo_rail_fact_driver \
  cargo build --locked --manifest-path "$manifest"

driver="$repository_root/tools/compiler-fact-driver/target/debug/cargo-rail-fact-driver"
driver_file="cargo-rail-fact-driver"
if [[ "${OS:-}" == Windows_NT ]]; then
  driver="$driver.exe"
  driver_file="$driver_file.exe"
fi
driver_digest="$(sha256_file "$driver")"
provenance="$({
  for file in \
    "$repository_root/tools/compiler-fact-driver/Cargo.lock" \
    "$repository_root/tools/compiler-fact-driver/Cargo.toml" \
    "$repository_root/tools/compiler-fact-driver/build.rs" \
    "$repository_root"/tools/compiler-fact-driver/src/*.rs \
    "$repository_root/src/compiler/fact_protocol.rs"; do
    printf '%s  %s\n' "$(sha256_file "$file")" "${file#"$repository_root"/}"
  done
} | sha256_stream)"
rustc_verbose="$(rustc -vV)"
rustc_release="$(printf '%s\n' "$rustc_verbose" | sed -n 's/^release: //p')"
rustc_commit="$(printf '%s\n' "$rustc_verbose" | sed -n 's/^commit-hash: //p')"
rustc_host="$(printf '%s\n' "$rustc_verbose" | sed -n 's/^host: //p')"
sysroot="$(rustc --print sysroot)"
if command -v cygpath >/dev/null 2>&1; then
  sysroot="$(cygpath -u "$sysroot")"
fi
compiler_libraries="$(find "$sysroot/lib" "$sysroot/bin" -maxdepth 1 -type f \( \
  -name 'librustc_driver-*.so' -o \
  -name 'rustc_driver-*.dll' \
\) 2>/dev/null || true)"
if [[ "$(printf '%s\n' "$compiler_libraries" | sed '/^$/d' | wc -l | tr -d ' ')" != 1 ]]; then
  printf 'expected exactly one rustc_driver runtime library under %s\n' "$sysroot" >&2
  exit 1
fi
compiler_library="$compiler_libraries"
compiler_library_relative="${compiler_library#"$sysroot"/}"
if [[ "$compiler_library_relative" == "$compiler_library" ]]; then
  echo "rustc_driver runtime library is outside the selected sysroot" >&2
  exit 1
fi
compiler_library_digest="$(sha256_file "$compiler_library")"

# Reproduce release archive topology: the embedded authority selects exactly
# one authenticated component beside the cargo-rail executable.
mkdir -p "$repository_root/target/debug"
staged_driver="$repository_root/target/debug/$driver_file"
cp "$driver" "$staged_driver"
if [[ "$(sha256_file "$staged_driver")" != "$driver_digest" ]]; then
  echo "staged compiler fact driver does not match its manufactured bytes" >&2
  exit 1
fi

unset CARGO_BUILD_TARGET CARGO_TARGET_DIR RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
export CARGO_RAIL_TEST_FACT_DRIVER="$driver"
export CARGO_RAIL_FACT_DRIVER_FILE="$driver_file"
export CARGO_RAIL_FACT_DRIVER_SHA256="sha256:$driver_digest"
export CARGO_RAIL_FACT_DRIVER_PROVENANCE="sha256:$provenance"
export CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE="$rustc_release"
export CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT="$rustc_commit"
export CARGO_RAIL_FACT_DRIVER_RUSTC_HOST="$rustc_host"
export CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY="$compiler_library_relative"
export CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256="sha256:$compiler_library_digest"

exec "$@"
