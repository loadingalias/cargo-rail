#!/usr/bin/env sh
if [ "${1:-}" != "GNU_LIBC_VERSION" ] || [ "$#" -ne 1 ]; then
  exit 2
fi
printf 'glibc %s\n' "${INSTALLER_TEST_GLIBC_VERSION:-2.39}"
