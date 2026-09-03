#!/usr/bin/env sh
set -eu

repository="https://github.com/loadingalias/cargo-rail"
embedded_version="@CARGO_RAIL_VERSION@"
version="${1:-$embedded_version}"
if [ "$version" = "$embedded_version" ]; then
  echo "usage: scripts/install.sh <exact-version>" >&2
  exit 2
fi
if [ "$#" -gt 1 ] || ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$'; then
  echo "version must be one exact release such as 1.2.3" >&2
  exit 2
fi

machine="$(uname -m)"
system="$(uname -s)"
gnu_runtime=false
case "$system-$machine" in
  Darwin-arm64) target="aarch64-apple-darwin"; surface=true ;;
  Linux-aarch64|Linux-arm64)
    if ldd --version 2>&1 | grep -qi musl; then
      target="aarch64-unknown-linux-musl"; surface=true
    else
      target="aarch64-unknown-linux-gnu"; surface=true
      gnu_runtime=true
    fi
    ;;
  Linux-riscv64)
    if ldd --version 2>&1 | grep -qi musl; then
      echo "no supported Cargo-Rail archive for $system $machine on musl" >&2
      exit 1
    fi
    target="riscv64gc-unknown-linux-gnu"
    surface=false
    gnu_runtime=true
    ;;
  Linux-x86_64)
    if ldd --version 2>&1 | grep -qi musl; then
      target="x86_64-unknown-linux-musl"; surface=true
    else
      target="x86_64-unknown-linux-gnu"; surface=true
      gnu_runtime=true
    fi
    ;;
  *) echo "no supported Cargo-Rail archive for $system $machine" >&2; exit 1 ;;
esac

gnu_minimum="2.39"
if [ "$gnu_runtime" = true ]; then
  command -v getconf >/dev/null 2>&1 || {
    echo "Cargo-Rail GNU archives require glibc $gnu_minimum or newer; getconf is unavailable" >&2
    exit 1
  }
  gnu_report="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
  gnu_version="$(printf '%s\n' "$gnu_report" | awk '$1 == "glibc" && NF == 2 { print $2 }')"
  printf '%s' "$gnu_version" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' || {
    echo "Cargo-Rail could not verify the host GNU libc version" >&2
    exit 1
  }
  if ! awk -v actual="$gnu_version" -v minimum="$gnu_minimum" 'BEGIN {
    split(actual, a, "."); split(minimum, m, ".");
    exit !((a[1] + 0 > m[1] + 0) || (a[1] + 0 == m[1] + 0 && a[2] + 0 >= m[2] + 0))
  }'; then
    echo "Cargo-Rail GNU archives require glibc $gnu_minimum or newer; found $gnu_version" >&2
    exit 1
  fi
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

archive="cargo-rail-$target.tar.gz"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl -fsSL --retry 3 "$repository/releases/download/v$version/$archive" -o "$temporary/$archive"
curl -fsSL --retry 3 "$repository/releases/download/v$version/SHA256SUMS" -o "$temporary/SHA256SUMS"

expected="$(awk -v file="$archive" '$2 == file || $2 == "*" file { print $1; exit }' "$temporary/SHA256SUMS")"
[ -n "$expected" ] || { echo "SHA256SUMS has no entry for $archive" >&2; exit 1; }
actual="$(sha256_file "$temporary/$archive")"
[ "$actual" = "$expected" ] || { echo "checksum mismatch for $archive" >&2; exit 1; }

mkdir "$temporary/extract"
tar -xzf "$temporary/$archive" -C "$temporary/extract"
manifest_matches="$(find "$temporary/extract" -type f -name cargo-rail-components-v1.tsv -print)"
[ "$(printf '%s\n' "$manifest_matches" | sed '/^$/d' | wc -l | tr -d ' ')" = 1 ] || {
  echo "$archive must contain exactly one component manifest" >&2
  exit 1
}
manifest="$manifest_matches"
component_directory="$(dirname "$manifest")"
header="$(sed -n '1p' "$manifest")"
[ "$header" = "cargo-rail-components-v1	$version	$target" ] || {
  echo "$archive component manifest has incompatible release authority" >&2
  exit 1
}

expected_names="cargo-rail
cargo-rail-compiler-observation
cargo-rail-distributed-worker
cargo-rail-native-rustc-worker
cargo-rail-native-rustc-wrapper"
if [ "$surface" = true ]; then
  expected_names="$expected_names
cargo-rail-fact-driver
cargo-rail-fact-driver-source-v1.json"
fi
declared_names="$(sed '1d' "$manifest" | cut -f1 | sort)"
[ "$declared_names" = "$(printf '%s\n' "$expected_names" | sort)" ] || {
  echo "$archive component manifest does not declare the exact platform inventory" >&2
  exit 1
}

tab="$(printf '\t')"
while IFS="$tab" read -r name digest bytes capability extra; do
  [ -n "$name" ] || continue
  [ -z "${extra:-}" ] || { echo "invalid component manifest entry" >&2; exit 1; }
  case "$name" in
    */*|*\\*|.*) echo "invalid component name in release manifest" >&2; exit 1 ;;
  esac
  printf '%s' "$digest" | grep -Eq '^[0-9a-f]{64}$' || {
    echo "invalid digest for $name" >&2; exit 1
  }
  printf '%s' "$bytes" | grep -Eq '^[0-9]+$' || {
    echo "invalid byte count for $name" >&2; exit 1
  }
  [ -n "$capability" ] || { echo "missing capability for $name" >&2; exit 1; }
  source="$component_directory/$name"
  [ -f "$source" ] && [ ! -L "$source" ] || { echo "$archive is missing $name" >&2; exit 1; }
  [ "$(wc -c < "$source" | tr -d ' ')" = "$bytes" ] || { echo "byte count mismatch for $name" >&2; exit 1; }
  [ "$(sha256_file "$source")" = "$digest" ] || { echo "digest mismatch for $name" >&2; exit 1; }
done <<EOF
$(sed '1d' "$manifest")
EOF

if [ -n "${CARGO_HOME:-}" ]; then
  install_directory="$CARGO_HOME/bin"
elif [ -n "${HOME:-}" ]; then
  install_directory="$HOME/.cargo/bin"
else
  echo "HOME or CARGO_HOME must be set" >&2
  exit 1
fi

mkdir -p "$install_directory"
stage="$(mktemp -d "$install_directory/.cargo-rail-install.XXXXXX")"
trap 'rm -rf "$temporary" "$stage"' EXIT HUP INT TERM
for name in $expected_names; do
  if [ "$name" = cargo-rail-fact-driver-source-v1.json ]; then
    install -m 600 "$component_directory/$name" "$stage/$name"
  else
    install -m 700 "$component_directory/$name" "$stage/$name"
  fi
done
install -m 600 "$manifest" "$stage/cargo-rail-components-v1.tsv"

for name in $expected_names; do
  mv -f "$stage/$name" "$install_directory/$name"
done
mv -f "$stage/cargo-rail-components-v1.tsv" "$install_directory/cargo-rail-components-v1.tsv"
rmdir "$stage"

actual_version="$("$install_directory/cargo-rail" rail --version)"
[ "$actual_version" = "cargo-rail $version" ] || {
  echo "installed binary reported '$actual_version', expected 'cargo-rail $version'" >&2
  exit 1
}

echo "Installed Cargo-Rail $version in $install_directory."
if [ "$surface" = true ]; then
  echo "Surface source authority is installed; exact-toolchain compiler support is prepared on first use."
else
  echo "Surface analysis is unavailable for $target."
fi
