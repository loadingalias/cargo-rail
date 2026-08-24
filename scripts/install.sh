#!/usr/bin/env sh
set -eu

repository="https://github.com/loadingalias/cargo-rail"
version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: scripts/install.sh <exact-version>" >&2
  exit 2
fi
case "$version" in
  *[!0-9.]*|.*|*.) echo "version must be an exact numeric release such as 0.22.1" >&2; exit 2 ;;
esac

machine="$(uname -m)"
system="$(uname -s)"
case "$system-$machine" in
  Darwin-arm64) target="aarch64-apple-darwin" ; surface=true ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ; surface=true ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-gnu" ; surface=true ;;
  Linux-x86_64)
    if ldd --version 2>&1 | grep -qi musl; then
      target="x86_64-unknown-linux-musl"
      surface=false
    else
      target="x86_64-unknown-linux-gnu"
      surface=true
    fi
    ;;
  *) echo "no supported Cargo-Rail native archive for $system $machine" >&2; exit 1 ;;
esac

archive="cargo-rail-$target.tar.gz"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl -fsSL --retry 3 "$repository/releases/download/v$version/$archive" -o "$temporary/$archive"
curl -fsSL --retry 3 "$repository/releases/download/v$version/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected="$(awk -v file="$archive" '$2 == file || $2 == "*" file { print $1; exit }' "$temporary/SHA256SUMS")"
[ -n "$expected" ] || { echo "SHA256SUMS has no entry for $archive" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary/$archive" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$temporary/$archive" | awk '{print $1}')"
fi
[ "$actual" = "$expected" ] || { echo "checksum mismatch for $archive" >&2; exit 1; }

mkdir "$temporary/extract"
tar -xzf "$temporary/$archive" -C "$temporary/extract"
install_directory="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$install_directory"
for executable in cargo-rail cargo-rail-compiler-observation cargo-rail-native-rustc-wrapper cargo-rail-native-rustc-worker cargo-rail-distributed-worker; do
  source="$(find "$temporary/extract" -type f -name "$executable" -print -quit)"
  [ -n "$source" ] || { echo "$archive is missing $executable" >&2; exit 1; }
  install -m 755 "$source" "$install_directory/$executable"
done
driver="$(find "$temporary/extract" -type f -name cargo-rail-fact-driver -print -quit)"
if [ "$surface" = true ]; then
  [ -n "$driver" ] || { echo "$archive is missing its surface companion" >&2; exit 1; }
  install -m 755 "$driver" "$install_directory/cargo-rail-fact-driver"
else
  [ -z "$driver" ] || { echo "musl archive unexpectedly contains an unsupported surface companion" >&2; exit 1; }
fi

"$install_directory/cargo-rail" rail --version
if [ "$surface" = true ]; then
  echo "surface: installed with adjacent authenticated companion"
else
  echo "surface: unavailable for $target"
fi
