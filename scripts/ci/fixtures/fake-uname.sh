#!/usr/bin/env sh
case "${1:-}" in
  -m) printf '%s\n' "$INSTALLER_TEST_MACHINE" ;;
  -s) printf '%s\n' "$INSTALLER_TEST_SYSTEM" ;;
  *) exit 2 ;;
esac
