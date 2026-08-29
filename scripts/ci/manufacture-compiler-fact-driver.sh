#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
readonly repository_root

usage() {
  echo "usage: $0 <target> <github-environment-file> <github-output-file> [component-directory]" >&2
  exit 2
}

[[ "$#" -ge 3 && "$#" -le 4 ]] || usage
target="$1"
environment_file="$2"
output_file="$3"
component_directory="${4:-$PWD}"
[[ "$target" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || usage
[[ -f "$environment_file" && ! -L "$environment_file" ]] || usage
[[ -f "$output_file" && ! -L "$output_file" ]] || usage
[[ -d "$component_directory" && ! -L "$component_directory" ]] || usage
component_directory="$(cd "$component_directory" && pwd -P)"

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

rustc_verbose="$(rustc -vV)"
host="$(sed -n 's/^host: //p' <<<"$rustc_verbose")"
[[ "$host" == "$target" ]] || {
  echo "compiler fact driver host '$host' does not match archive target '$target'" >&2
  exit 1
}
: "${RUSTUP_TOOLCHAIN:?selected Rustup toolchain is missing}"
rustup component add rustc-dev --toolchain "$RUSTUP_TOOLCHAIN"

target_dir="${RUNNER_TEMP:-$repository_root/target}/cargo-rail-fact-driver-$target"
driver_flags=""
case "$target" in
  aarch64-unknown-linux-musl|x86_64-unknown-linux-musl)
    # rustc_driver is a shared compiler library. The companion must use the
    # native musl loader; the core Cargo-Rail archive retains static CRT policy.
    driver_flags="-C target-feature=-crt-static"
    ;;
esac
env -u CARGO_ENCODED_RUSTFLAGS \
  CARGO_TARGET_DIR="$target_dir" \
  RUSTFLAGS="$driver_flags" \
  scripts/build-compiler-fact-driver.sh --target "$target"

suffix=""
if [[ "$target" == *-windows-* ]]; then
  suffix=".exe"
fi
source="$target_dir/$target/release/cargo-rail-fact-driver${suffix}"
component="cargo-rail-fact-driver${suffix}"
component_path="$component_directory/$component"
cp "$source" "$component_path"
digest="$(sha256_file "$component_path")"

source_bundle="cargo-rail-fact-driver-source-v1.json"
source_bundle_path="$component_directory/$source_bundle"
scripts/package-compiler-fact-driver-source.py "$source_bundle_path"
source_bundle_digest="$(sha256_file "$source_bundle_path")"
provenance="$({
  for file in \
    tools/compiler-fact-driver/Cargo.lock \
    tools/compiler-fact-driver/Cargo.toml \
    tools/compiler-fact-driver/build.rs \
    tools/compiler-fact-driver/src/*.rs \
    src/compiler/fact_protocol.rs; do
    printf '%s  %s\n' "$(sha256_file "$file")" "$file"
  done
} | sha256_stream)"
release="$(sed -n 's/^release: //p' <<<"$rustc_verbose")"
commit="$(sed -n 's/^commit-hash: //p' <<<"$rustc_verbose")"
sysroot="$(rustc --print sysroot)"
if command -v cygpath >/dev/null 2>&1; then
  sysroot="$(cygpath -u "$sysroot")"
fi
compiler_libraries="$(find "$sysroot/lib" "$sysroot/bin" -maxdepth 1 -type f \( \
  -name 'librustc_driver-*.so' -o \
  -name 'librustc_driver-*.dylib' -o \
  -name 'rustc_driver-*.dll' \
\) 2>/dev/null || true)"
[[ "$(printf '%s\n' "$compiler_libraries" | sed '/^$/d' | wc -l | tr -d ' ')" == 1 ]] || {
  echo "expected exactly one rustc_driver runtime library under $sysroot" >&2
  exit 1
}
compiler_library="$compiler_libraries"
compiler_library_relative="${compiler_library#"$sysroot"/}"
[[ "$compiler_library_relative" != "$compiler_library" ]] || {
  echo "rustc_driver runtime library is outside the selected sysroot" >&2
  exit 1
}
compiler_library_digest="$(sha256_file "$compiler_library")"

case "$target" in
  aarch64-unknown-linux-musl)
    interpreter=/lib/ld-musl-aarch64.so.1
    ;;
  x86_64-unknown-linux-musl)
    interpreter=/lib/ld-musl-x86_64.so.1
    ;;
  *)
    interpreter=""
    ;;
esac
if [[ -n "$interpreter" ]]; then
  command -v readelf >/dev/null 2>&1 || {
    echo "musl compiler fact driver qualification requires readelf" >&2
    exit 2
  }
  [[ -x "$interpreter" ]] || {
    echo "native musl interpreter is unavailable: $interpreter" >&2
    exit 1
  }
  readelf -l "$component_path" | grep -Fq "Requesting program interpreter: $interpreter" || {
    echo "compiler fact driver does not select the native musl interpreter $interpreter" >&2
    exit 1
  }
  protocol="$(
    LD_LIBRARY_PATH="$(dirname "$compiler_library")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
      "$component_path" --cargo-rail-fact-protocol-version
  )"
  [[ "$protocol" =~ ^[1-9][0-9]*$ ]] || {
    echo "native musl compiler fact driver emitted invalid protocol '$protocol'" >&2
    exit 1
  }
fi

{
  echo "CARGO_RAIL_FACT_DRIVER_FILE=$component"
  echo "CARGO_RAIL_FACT_DRIVER_SHA256=sha256:$digest"
  echo "CARGO_RAIL_FACT_DRIVER_PROVENANCE=sha256:$provenance"
  echo "CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE=$release"
  echo "CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT=$commit"
  echo "CARGO_RAIL_FACT_DRIVER_RUSTC_HOST=$host"
  echo "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY=$compiler_library_relative"
  echo "CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256=sha256:$compiler_library_digest"
  echo "CARGO_RAIL_FACT_DRIVER_SOURCE_FILE=$source_bundle"
  echo "CARGO_RAIL_FACT_DRIVER_SOURCE_SHA256=sha256:$source_bundle_digest"
  echo "CARGO_RAIL_FACT_DRIVER_SOURCE_PROVENANCE=sha256:$provenance"
} >>"$environment_file"
echo "include=$component,$source_bundle" >>"$output_file"
