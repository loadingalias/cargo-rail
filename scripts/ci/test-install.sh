#!/usr/bin/env bash
set -euo pipefail

temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-installer-test.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

version="9.8.7"
release="$temporary/repository/releases/download/v$version"
mkdir -p "$release" "$temporary/payload"

targets=(
  "Darwin arm64 native aarch64-apple-darwin true"
  "Linux aarch64 gnu aarch64-unknown-linux-gnu true"
  "Linux aarch64 musl aarch64-unknown-linux-musl false"
  "Linux x86_64 gnu x86_64-unknown-linux-gnu true"
  "Linux x86_64 musl x86_64-unknown-linux-musl false"
)
# Native Windows jq uses CRLF records. Target identity does not include the record terminator.
diff -u \
  <(jq -r '.[] | select(.target | endswith("-pc-windows-msvc") | not) | .target' \
    distribution/release-targets.json | sed 's/\r$//' | LC_ALL=C sort) \
  <(printf '%s\n' "${targets[@]}" | awk '{print $4}' | LC_ALL=C sort)

for specification in "${targets[@]}"; do
  read -r system machine libc target surface <<< "$specification"
  payload="$temporary/payload/cargo-rail-$target"
  mkdir "$payload"
  executables=(
    cargo-rail
    cargo-rail-compiler-observation
    cargo-rail-native-rustc-wrapper
    cargo-rail-native-rustc-worker
    cargo-rail-distributed-worker
  )
  if [ "$surface" = true ]; then
    executables+=(cargo-rail-fact-driver)
    printf '{"version":1,"files":[]}' > "$payload/cargo-rail-fact-driver-source-v1.json"
  fi
  for executable in "${executables[@]}"; do
    cp scripts/ci/fixtures/fake-cargo-rail-installer-binary.sh "$payload/$executable"
    chmod +x "$payload/$executable"
  done

  archive="cargo-rail-$target.tar.gz"
  tar -czf "$release/$archive" -C "$temporary/payload" "cargo-rail-$target"
  scripts/package-release-archive.py "$release/$archive" \
    --target "$target" --version "$version" --surface "$surface"
  if command -v sha256sum >/dev/null 2>&1; then
    digest="$(sha256sum "$release/$archive" | awk '{print $1}')"
  else
    digest="$(shasum -a 256 "$release/$archive" | awk '{print $1}')"
  fi
  printf '%s  %s\n' "$digest" "$archive" >> "$release/SHA256SUMS"
done

installer="$temporary/install.sh"
repository_url="file://$temporary/repository"
if command -v cygpath >/dev/null 2>&1; then
  repository_url="file:///$(cygpath -m "$temporary/repository")"
fi
sed "s#repository=\"https://github.com/loadingalias/cargo-rail\"#repository=\"$repository_url\"#" \
  scripts/install.sh > "$installer"
chmod +x "$installer"

mkdir "$temporary/bin"
cp scripts/ci/fixtures/fake-uname.sh "$temporary/bin/uname"
cp scripts/ci/fixtures/fake-ldd.sh "$temporary/bin/ldd"
chmod +x "$temporary/bin/uname" "$temporary/bin/ldd"

for specification in "${targets[@]}"; do
  read -r system machine libc target surface <<< "$specification"
  cargo_home="$temporary/cargo-home-$target"
  stdout="$temporary/stdout-$target"
  INSTALLER_TEST_SYSTEM="$system" INSTALLER_TEST_MACHINE="$machine" INSTALLER_TEST_LIBC="$libc" \
    PATH="$temporary/bin:$PATH" CARGO_HOME="$cargo_home" \
    "$installer" "$version" > "$stdout"
  grep -Fq "Installed Cargo-Rail $version" "$stdout"
  for executable in cargo-rail cargo-rail-compiler-observation cargo-rail-native-rustc-wrapper \
    cargo-rail-native-rustc-worker cargo-rail-distributed-worker; do
    test -x "$cargo_home/bin/$executable"
  done
  if [ "$surface" = true ]; then
    test -x "$cargo_home/bin/cargo-rail-fact-driver"
    test -f "$cargo_home/bin/cargo-rail-fact-driver-source-v1.json"
  else
    test ! -e "$cargo_home/bin/cargo-rail-fact-driver"
    test ! -e "$cargo_home/bin/cargo-rail-fact-driver-source-v1.json"
  fi
  test -f "$cargo_home/bin/cargo-rail-components-v1.tsv"
  printf 'damaged' > "$cargo_home/bin/cargo-rail-native-rustc-worker"
  INSTALLER_TEST_SYSTEM="$system" INSTALLER_TEST_MACHINE="$machine" INSTALLER_TEST_LIBC="$libc" \
    PATH="$temporary/bin:$PATH" CARGO_HOME="$cargo_home" "$installer" "$version" > /dev/null
  cmp scripts/ci/fixtures/fake-cargo-rail-installer-binary.sh "$cargo_home/bin/cargo-rail-native-rustc-worker"
done

if INSTALLER_TEST_SYSTEM=Darwin INSTALLER_TEST_MACHINE=x86_64 INSTALLER_TEST_LIBC=native \
  PATH="$temporary/bin:$PATH" CARGO_HOME="$temporary/unsupported-macos-home" \
  "$installer" "$version" > /dev/null 2> "$temporary/unsupported-macos-stderr"; then
  echo "installer accepted unsupported Intel macOS" >&2
  exit 1
fi
grep -Fq "no supported Cargo-Rail archive for Darwin x86_64" "$temporary/unsupported-macos-stderr"
test ! -e "$temporary/unsupported-macos-home/bin/cargo-rail"

bad_archive="cargo-rail-x86_64-unknown-linux-gnu.tar.gz"
printf '%064d  %s\n' 0 "$bad_archive" > "$release/SHA256SUMS"
if INSTALLER_TEST_SYSTEM=Linux INSTALLER_TEST_MACHINE=x86_64 INSTALLER_TEST_LIBC=gnu \
  PATH="$temporary/bin:$PATH" RUSTUP_LOG="$temporary/bad-rustup.log" CARGO_HOME="$temporary/bad-home" \
  "$installer" "$version" > /dev/null 2> "$temporary/stderr"; then
  echo "installer accepted an invalid archive checksum" >&2
  exit 1
fi
grep -Fq "checksum mismatch for $bad_archive" "$temporary/stderr"
test ! -e "$temporary/bad-home/bin/cargo-rail"

mkdir "$temporary/tampered-payload"
tar -xzf "$release/$bad_archive" -C "$temporary/tampered-payload"
tampered_component="$temporary/tampered-payload/cargo-rail-x86_64-unknown-linux-gnu/cargo-rail-native-rustc-worker"
component_bytes="$(wc -c < "$tampered_component" | tr -d ' ')"
dd if=/dev/zero bs="$component_bytes" count=1 2>/dev/null | tr '\000' x > "$tampered_component"
tar -czf "$release/$bad_archive" -C "$temporary/tampered-payload" cargo-rail-x86_64-unknown-linux-gnu
printf '%s  %s\n' "$(if command -v sha256sum >/dev/null 2>&1; then sha256sum "$release/$bad_archive"; else shasum -a 256 "$release/$bad_archive"; fi | awk '{print $1}')" \
  "$bad_archive" > "$release/SHA256SUMS"
if INSTALLER_TEST_SYSTEM=Linux INSTALLER_TEST_MACHINE=x86_64 INSTALLER_TEST_LIBC=gnu \
  PATH="$temporary/bin:$PATH" CARGO_HOME="$temporary/tampered-home" \
  "$installer" "$version" > /dev/null 2> "$temporary/tampered-stderr"; then
  echo "installer accepted component bytes that disagreed with the authenticated inventory" >&2
  exit 1
fi
if ! grep -Fq "digest mismatch for cargo-rail-native-rustc-worker" "$temporary/tampered-stderr"; then
  echo "installer reported the wrong component-integrity failure:" >&2
  sed 's/^/  /' "$temporary/tampered-stderr" >&2
  exit 1
fi
test ! -e "$temporary/tampered-home/bin/cargo-rail"
