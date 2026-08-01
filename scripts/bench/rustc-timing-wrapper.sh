#!/usr/bin/env bash
set -uo pipefail

timing_dir="${CARGO_RAIL_UNIT_TIMINGS:?CARGO_RAIL_UNIT_TIMINGS must name an output directory}"
workspace="${CARGO_RAIL_BENCH_WORKSPACE:?CARGO_RAIL_BENCH_WORKSPACE must name the fixture root}"
mkdir -p "$timing_dir"
record="$timing_dir/unit-$$"

crate_name="unknown"
crate_type="unknown"
emit="unknown"
source="unknown"
test_mode="false"
native_link="false"
incremental="false"
link_probe="not_requested"
linker_producing="false"

arguments=("$@")
index=1
while ((index < ${#arguments[@]})); do
  argument="${arguments[$index]}"
  next=""
  if ((index + 1 < ${#arguments[@]})); then
    next="${arguments[$((index + 1))]}"
  fi
  case "$argument" in
    --crate-name)
      crate_name="$next"
      ((index += 2))
      continue
      ;;
    --crate-name=*) crate_name="${argument#--crate-name=}" ;;
    --crate-type)
      crate_type="$next"
      ((index += 2))
      continue
      ;;
    --crate-type=*) crate_type="${argument#--crate-type=}" ;;
    --emit)
      emit="$next"
      ((index += 2))
      continue
      ;;
    --emit=*) emit="${argument#--emit=}" ;;
    --test) test_mode="true" ;;
    -L)
      if [[ "$next" == native=* ]]; then
        native_link="true"
      fi
      ((index += 2))
      continue
      ;;
    -l | -l* | -Lnative* | *linker=* | *link-arg=* | *link-args=*) native_link="true" ;;
    *incremental=*) incremental="true" ;;
    *.rs)
      if [[ "$source" == unknown ]]; then
        source="$argument"
      fi
      ;;
  esac
  ((index += 1))
done

case ",$crate_type," in
  *,bin,* | *,proc-macro,* | *,dylib,* | *,cdylib,* | *,staticlib,*) linker_producing="true" ;;
esac

case "$source" in
  "$workspace"/*) source_class="workspace" ;;
  */registry/src/*) source_class="registry" ;;
  */git/checkouts/*) source_class="git" ;;
  /*) source_class="host" ;;
  *.rs) source_class="workspace" ;;
  *) source_class="host" ;;
esac

printf '%s\0' "$@" >"$record.argv"
if [[ "${CARGO_RAIL_LINK_PROBE:-0}" == 1 && "$OSTYPE" == darwin* && "$linker_producing" == true && \
  "$emit" == *link* && "$test_mode" == false ]]; then
  link_dependencies="$record.link-dependencies"
  link_stdout="$record.link-stdout"
  if [[ "$link_dependencies" == *,* ]]; then
    link_probe="unsupported_path"
    /usr/bin/time -p -o "$record.time" "$@"
    status=$?
  else
    /usr/bin/time -p -o "$record.time" "$@" --print=link-args \
      "-Clink-arg=-Wl,-dependency_info,$link_dependencies" >"$link_stdout"
    status=$?
    if [[ "$status" -eq 0 && -s "$link_dependencies" && "$(head -c 4 "$link_stdout")" == "env " ]]; then
      head -n 1 "$link_stdout" >"$record.link-command"
      tail -n +2 "$link_stdout"
      link_probe="captured"
    else
      cat "$link_stdout"
      link_probe="incomplete"
    fi
  fi
else
  /usr/bin/time -p -o "$record.time" "$@"
  status=$?
fi
real="$(awk '$1 == "real" { print $2 }' "$record.time")"
user="$(awk '$1 == "user" { print $2 }' "$record.time")"
sys="$(awk '$1 == "sys" { print $2 }' "$record.time")"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "$crate_name" "$crate_type" "$emit" "$source_class" "$test_mode" "$native_link" "$incremental" \
  "${real:-0}" "${user:-0}" "${sys:-0}" "$status" "$link_probe" >"$record.tsv"
exit "$status"
