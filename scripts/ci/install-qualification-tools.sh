#!/usr/bin/env bash
set -euo pipefail

# Detects the qualification profile for the current host and installs its
# tools via install-tools.sh. Remote qualification machines are always Linux
# or Windows; this script only selects which profile applies.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

host="$(rustc -vV | sed -n 's/^host: //p')"
case "$host" in
  *-unknown-linux-*) profile=linux-qualification ;;
  *-pc-windows-*) profile=windows-qualification ;;
  *)
    echo "remote qualification requires a supported Linux or Windows host, got: $host" >&2
    exit 2
    ;;
esac

exec "$script_dir/install-tools.sh" "$profile"
