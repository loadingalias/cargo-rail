#!/usr/bin/env bash
set -euo pipefail

: "${FAKE_EXPECTED_RUSTUP_HOME:?}"
[[ "${RUSTUP_HOME:-}" == "$FAKE_EXPECTED_RUSTUP_HOME" ]] || {
  echo "rustup was called outside the isolated test home" >&2
  exit 2
}

printf '%s\n' "$*" >>"$FAKE_RUSTUP_LOG"

case "${1:-}" in
  toolchain) exit 0 ;;
  which)
    printf '%s\n' "/fixture/${4:-unknown}"
    ;;
  run)
    case "${3:-}" in
      rustc)
        if [[ "${4:-}" == --version && "${5:-}" == --verbose ]]; then
          printf '%s\n' \
            'rustc 1.98.0 (fixture 2026-08-18)' \
            'binary: rustc' \
            'commit-hash: fixture' \
            'commit-date: 2026-08-18' \
            "host: ${FAKE_RUSTC_HOST:-aarch64-pc-windows-msvc}" \
            'release: 1.98.0' \
            'LLVM version: fixture'
        else
          printf '%s\n' 'rustc 1.98.0 (fixture 2026-08-18)'
        fi
        ;;
      cargo) printf '%s\n' 'cargo 1.98.0 (fixture 2026-08-18)' ;;
      rustdoc) printf '%s\n' 'rustdoc 1.98.0 (fixture 2026-08-18)' ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
