#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-rust-toolchain-test.XXXXXX")"
trap 'rm -rf -- "$temporary"' EXIT

mkdir -p "$temporary/bin" "$temporary/runner-temp"
cp "$script_dir/fixtures/fake-ci-rustup.sh" "$temporary/bin/rustup"
chmod +x "$temporary/bin/rustup"
: >"$temporary/github-env"
: >"$temporary/rustup.log"
bash_binary="$(command -v bash)"
test_path="$temporary/bin:$(dirname "$bash_binary"):/usr/bin:/bin"

env -i \
  PATH="$test_path" \
  GITHUB_ENV="$temporary/github-env" \
  RUNNER_TEMP="$temporary/runner-temp" \
  FAKE_EXPECTED_RUSTUP_HOME="$temporary/runner-temp/cargo-rail-rustup" \
  FAKE_RUSTUP_LOG="$temporary/rustup.log" \
  "$bash_binary" --noprofile --norc "$script_dir/install-rust-toolchain.sh" \
    --components clippy,rustfmt \
    --targets thumbv7em-none-eabihf,wasm32-unknown-unknown >/dev/null

expected_home="$temporary/runner-temp/cargo-rail-rustup"
grep -Fxq \
  'toolchain install 1.98.0 --profile minimal --no-self-update --component cargo --component clippy --component rustfmt --target thumbv7em-none-eabihf --target wasm32-unknown-unknown' \
  "$temporary/rustup.log"
grep -Fxq "RUSTUP_HOME=$expected_home" "$temporary/github-env"
grep -Fxq 'RUSTUP_TOOLCHAIN=1.98.0-aarch64-pc-windows-msvc' "$temporary/github-env"
grep -Fxq 'RUSTUP_AUTO_INSTALL=0' "$temporary/github-env"
grep -Fxq 'run 1.98.0-aarch64-pc-windows-msvc cargo --version' "$temporary/rustup.log"
grep -Fxq 'run 1.98.0-aarch64-pc-windows-msvc rustc --version' "$temporary/rustup.log"
grep -Fxq 'run 1.98.0-aarch64-pc-windows-msvc rustdoc --version' "$temporary/rustup.log"

if env -i \
  PATH="$test_path" \
  GITHUB_ENV="$temporary/github-env" \
  RUNNER_TEMP="$temporary/runner-temp" \
  FAKE_EXPECTED_RUSTUP_HOME="$temporary/runner-temp/cargo-rail-rustup" \
  FAKE_RUSTUP_LOG="$temporary/rustup.log" \
  "$bash_binary" --noprofile --norc "$script_dir/install-rust-toolchain.sh" \
    --toolchain stable >/dev/null 2>&1; then
  echo "CI Rust installer accepted a non-exact toolchain" >&2
  exit 1
fi

if env -i \
  PATH="$test_path" \
  GITHUB_ENV="$temporary/github-env" \
  RUNNER_TEMP="$temporary/runner-temp" \
  FAKE_EXPECTED_RUSTUP_HOME="$temporary/runner-temp/cargo-rail-rustup" \
  FAKE_RUSTUP_LOG="$temporary/rustup.log" \
  "$bash_binary" --noprofile --norc "$script_dir/install-rust-toolchain.sh" \
    --components clippy, >/dev/null 2>&1; then
  echo "CI Rust installer accepted an empty component" >&2
  exit 1
fi

echo "CI Rust toolchain installer regression tests passed"
