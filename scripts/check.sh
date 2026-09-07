#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

[[ "$#" -eq 1 ]] || { echo 'usage: scripts/check.sh {fix|local|native}' >&2; exit 64; }
mode="$1"
case "$mode" in
  fix|local)
    [[ "$(uname -s)" == Darwin ]] || { echo 'use just ci-check for native checks outside the macOS workstation' >&2; exit 1; }
    for tool in zig cargo-zigbuild cargo-xwin; do
      command -v "$tool" >/dev/null || { echo "missing local check prerequisite: $tool" >&2; exit 127; }
    done
    ;;
  native) ;;
  *) echo 'usage: scripts/check.sh {fix|local|native}' >&2; exit 64 ;;
esac

args=(--workspace --all-targets --all-features --locked)
if [[ "$mode" == fix ]]; then
  cargo fmt --all
  # Local repair must work in the edited worktree, including staged changes.
  args+=(--fix --allow-dirty --allow-staged)
else
  cargo fmt --all -- --check
fi

echo "Clippy ($mode): native host"
cargo clippy "${args[@]}"
if [[ "$mode" != native ]]; then
  for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
                x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    echo "Clippy ($mode): $target"
    cargo-zigbuild clippy --target "$target" "${args[@]}"
  done
  for target in x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
    echo "Clippy ($mode): $target"
    cargo xwin clippy --target "$target" "${args[@]}"
  done
fi

if [[ "$mode" == fix ]]; then
  # Clippy can introduce edits after the initial formatting pass.
  cargo fmt --all
  exit 0
fi

if [[ "$mode" == local ]]; then
  cargo rail unify --check --explain
fi
deny_args=(--locked --workspace --all-features)
if [[ "$mode" == native ]]; then
  host="$(rustc -vV | sed -n 's/^host: //p')"
  deny_args+=(--target "$host")
fi
cargo deny "${deny_args[@]}" check -D warnings all
RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings" cargo doc --workspace --no-deps --all-features --locked
