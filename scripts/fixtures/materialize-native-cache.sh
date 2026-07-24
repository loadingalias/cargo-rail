#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <destination> [shared-git-source]" >&2
  exit 2
}

[[ $# -ge 1 && $# -le 2 ]] || usage
destination="$1"
[[ -n "$destination" && "$destination" != -* ]] || usage
shared_git_source="${2:-${destination}.git-source}"
[[ -n "$shared_git_source" && "$shared_git_source" != -* ]] || usage

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
template="$repo_root/tests/fixtures/native_cache/real_world"

if [[ -e "$destination" ]]; then
  [[ -d "$destination" ]] || { echo "fixture destination is not a directory: $destination" >&2; exit 2; }
else
  mkdir -p "$destination"
fi
destination="$(cd "$destination" && pwd -P)"
[[ -z "$(find "$destination" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
  echo "fixture destination must be empty: $destination" >&2
  exit 2
}

cp -R "$template/." "$destination"

git_source_template="$destination/git-source"
if [[ -e "$shared_git_source" ]]; then
  [[ -d "$shared_git_source/.git" ]] || {
    echo "shared Git source is not a fixture repository: $shared_git_source" >&2
    exit 2
  }
  rm -r -- "$git_source_template"
else
  mkdir -p "$(dirname "$shared_git_source")"
  mv "$git_source_template" "$shared_git_source"
  git -C "$shared_git_source" init --quiet --initial-branch=main --object-format=sha1
  git -C "$shared_git_source" config core.autocrlf false
  git -C "$shared_git_source" config core.filemode false
  git -C "$shared_git_source" add --all
  git_tree="$(git -C "$shared_git_source" write-tree)"
  git_commit="$({
    GIT_AUTHOR_NAME='cargo-rail fixture' \
    GIT_AUTHOR_EMAIL='fixture@cargo-rail.invalid' \
    GIT_AUTHOR_DATE='2000-01-01T00:00:00Z' \
    GIT_COMMITTER_NAME='cargo-rail fixture' \
    GIT_COMMITTER_EMAIL='fixture@cargo-rail.invalid' \
    GIT_COMMITTER_DATE='2000-01-01T00:00:00Z' \
      git -C "$shared_git_source" commit-tree "$git_tree" -m 'Create native-cache Git dependency'
  })"
  git -C "$shared_git_source" update-ref refs/heads/main "$git_commit"
fi
git_source="$(cd "$shared_git_source" && pwd -P)"
git_commit="$(git -C "$git_source" rev-parse --verify HEAD)"

manifest="$destination/Cargo.toml"
rendered="$destination/Cargo.toml.rendered"
git_url="file://$git_source"
sed -e "s|__FIXTURE_GIT_URL__|$git_url|" -e "s|__FIXTURE_GIT_REV__|$git_commit|" "$manifest" >"$rendered"
mv "$rendered" "$manifest"

prefetch_manifest="$destination/git-prefetch/Cargo.toml"
prefetch_rendered="$destination/git-prefetch/Cargo.toml.rendered"
sed -e "s|__FIXTURE_GIT_URL__|$git_url|" -e "s|__FIXTURE_GIT_REV__|$git_commit|" \
  "$prefetch_manifest" >"$prefetch_rendered"
mv "$prefetch_rendered" "$prefetch_manifest"
cargo metadata --manifest-path "$prefetch_manifest" --format-version=1 >/dev/null
rm -r -- "$destination/git-prefetch"

cargo generate-lockfile --manifest-path "$manifest" --offline --quiet
git -C "$destination" init --quiet --initial-branch=main --object-format=sha1
git -C "$destination" config core.autocrlf false
git -C "$destination" config core.filemode false
git -C "$destination" add --all
fixture_tree="$(git -C "$destination" write-tree)"
fixture_commit="$({
  GIT_AUTHOR_NAME='cargo-rail fixture' \
  GIT_AUTHOR_EMAIL='fixture@cargo-rail.invalid' \
  GIT_AUTHOR_DATE='2000-01-02T00:00:00Z' \
  GIT_COMMITTER_NAME='cargo-rail fixture' \
  GIT_COMMITTER_EMAIL='fixture@cargo-rail.invalid' \
  GIT_COMMITTER_DATE='2000-01-02T00:00:00Z' \
    git -C "$destination" commit-tree "$fixture_tree" -m 'Materialize native-cache workload'
})"
git -C "$destination" update-ref refs/heads/main "$fixture_commit"

echo "materialized native-cache fixture at $destination" >&2
