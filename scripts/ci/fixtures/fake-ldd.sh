#!/usr/bin/env sh
if [ "${INSTALLER_TEST_LIBC:-gnu}" = "musl" ]; then
  echo "musl libc"
else
  echo "GNU libc"
fi
