#!/usr/bin/env bash
set -euo pipefail

real_driver="${CARGO_RAIL_LINK_DRIVER:?CARGO_RAIL_LINK_DRIVER must name the real linker driver}"
certificate="${CARGO_RAIL_LINK_CERTIFICATE:?CARGO_RAIL_LINK_CERTIFICATE must name the dependency certificate}"
command_record="${CARGO_RAIL_LINK_COMMAND_RECORD:-}"

[[ "$OSTYPE" == darwin* ]] || {
  echo "the Apple linker probe only supports Darwin" >&2
  exit 86
}
[[ "$real_driver" == /* && -x "$real_driver" ]] || {
  echo "the Apple linker probe requires an absolute executable driver" >&2
  exit 86
}
[[ "$certificate" == /* && "$certificate" != *,* ]] || {
  echo "the Apple linker probe requires an absolute comma-free certificate path" >&2
  exit 86
}

temporary_prefix=""
for argument in "$@"; do
  if [[ "$argument" =~ ^(.*/rustc[[:alnum:]]{6})/[^/]+\.(rlib|a)$ ]]; then
    candidate="${BASH_REMATCH[1]}"
    if [[ -n "$temporary_prefix" && "$temporary_prefix" != "$candidate" ]]; then
      echo "the Apple linker probe observed multiple rustc temporary roots" >&2
      exit 86
    fi
    temporary_prefix="$candidate"
  fi
done

if [[ -n "$command_record" ]]; then
  [[ "$command_record" == /* ]] || {
    echo "the Apple linker probe requires an absolute command-record path" >&2
    exit 86
  }
  printf '%s\0' "$real_driver" "$@" >"$command_record"
fi

link_arguments=("-Wl,-dependency_info,$certificate")
if [[ -n "$temporary_prefix" ]]; then
  link_arguments+=("-Wl,-oso_prefix,$temporary_prefix/")
fi
exec "$real_driver" "$@" "${link_arguments[@]}"
