#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <member-count> <destination>" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage

member_count="$1"
destination="$2"

[[ "$member_count" =~ ^[1-9][0-9]*$ ]] || {
  echo "member count must be a positive integer: $member_count" >&2
  exit 2
}
[[ -n "$destination" && "$destination" != -* ]] || {
  echo "fixture destination must be a nonempty path and cannot start with '-': $destination" >&2
  exit 2
}

export LC_ALL=C
umask 022

for tool in cargo git; do
  command -v "$tool" >/dev/null || {
    echo "missing required fixture tool: $tool" >&2
    exit 2
  }
done

if [[ -e "$destination" ]]; then
  [[ -d "$destination" ]] || {
    echo "fixture destination is not a directory: $destination" >&2
    exit 2
  }
else
  mkdir -p "$destination"
fi

destination="$(cd "$destination" && pwd -P)"
[[ -z "$(find "$destination" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
  echo "fixture destination must be empty: $destination" >&2
  exit 2
}

mkdir -p "$destination/.config" "$destination/crates"

printf '%s\n' \
  '[workspace]' \
  'resolver = "3"' \
  'members = ["crates/*"]' \
  '' \
  '[workspace.package]' \
  'version = "0.1.0"' \
  'edition = "2024"' \
  'rust-version = "1.95.0"' \
  'license = "MIT"' \
  >"$destination/Cargo.toml"

printf '%s\n' \
  '[workspace]' \
  'root = "."' \
  '' \
  '[toolchain]' \
  'channel = "stable"' \
  >"$destination/.config/rail.toml"

printf '%s\n' 'target/' >"$destination/.gitignore"
printf '%s\n' '* text eol=lf -filter -ident -working-tree-encoding' >"$destination/.gitattributes"

for ((index = 0; index < member_count; index++)); do
  printf -v member 'member-%04d' "$index"
  crate_dir="$destination/crates/$member"
  mkdir -p "$crate_dir/src"

  {
    printf '%s\n' \
      '[package]' \
      "name = \"$member\"" \
      'version.workspace = true' \
      'edition.workspace = true' \
      'rust-version.workspace = true' \
      'license.workspace = true' \
      'publish = false' \
      '' \
      '[dependencies]'

    if ((index > 0)); then
      printf -v previous 'member-%04d' "$((index - 1))"
      printf '%s = { path = "../%s" }\n' "$previous" "$previous"
    fi
  } >"$crate_dir/Cargo.toml"

  if ((index == 0)); then
    printf 'pub fn value() -> usize { 0 }\n' >"$crate_dir/src/lib.rs"
  else
    previous_rust="${previous//-/_}"
    printf 'pub fn value() -> usize { %s::value() + 1 }\n' "$previous_rust" >"$crate_dir/src/lib.rs"
  fi
done

cargo generate-lockfile --offline --quiet --manifest-path "$destination/Cargo.toml"

git -C "$destination" init --quiet --initial-branch=main --object-format=sha1
git -C "$destination" config core.autocrlf false
git -C "$destination" config core.filemode false
git -C "$destination" add --all

# Build commits directly so fixture generation never executes ambient Git hooks.
initial_tree="$(git -C "$destination" write-tree)"
initial_commit="$(
  GIT_AUTHOR_NAME='cargo-rail fixture' \
  GIT_AUTHOR_EMAIL='fixture@cargo-rail.invalid' \
  GIT_AUTHOR_DATE='2000-01-01T00:00:00Z' \
  GIT_COMMITTER_NAME='cargo-rail fixture' \
  GIT_COMMITTER_EMAIL='fixture@cargo-rail.invalid' \
  GIT_COMMITTER_DATE='2000-01-01T00:00:00Z' \
    git -C "$destination" commit-tree "$initial_tree" -m 'Generate workspace fixture'
)"
git -C "$destination" update-ref refs/heads/main "$initial_commit"

printf 'pub fn value() -> usize { 1 }\n' >"$destination/crates/member-0000/src/lib.rs"
git -C "$destination" add crates/member-0000/src/lib.rs

changed_tree="$(git -C "$destination" write-tree)"
changed_commit="$(
  GIT_AUTHOR_NAME='cargo-rail fixture' \
  GIT_AUTHOR_EMAIL='fixture@cargo-rail.invalid' \
  GIT_AUTHOR_DATE='2000-01-02T00:00:00Z' \
  GIT_COMMITTER_NAME='cargo-rail fixture' \
  GIT_COMMITTER_EMAIL='fixture@cargo-rail.invalid' \
  GIT_COMMITTER_DATE='2000-01-02T00:00:00Z' \
    git -C "$destination" commit-tree "$changed_tree" -p "$initial_commit" -m 'Change member-0000'
)"
git -C "$destination" update-ref refs/heads/main "$changed_commit"

echo "generated $member_count-member fixture at $destination" >&2
