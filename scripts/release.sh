#!/usr/bin/env bash
# Run the source-built `cargo rail release --yes` in GitHub Actions and publish its result.
#
# The token reaches Git only through process-scoped configuration for this one command,
# and the release commit gets the Actions bot identity when the checkout has none.
set -euo pipefail

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_OUTPUT:?run inside GitHub Actions}"
server="${GITHUB_SERVER_URL:-https://github.com}"
header="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')"
if ! git config user.email >/dev/null; then
  export GIT_AUTHOR_NAME='github-actions[bot]' GIT_COMMITTER_NAME='github-actions[bot]'
  export GIT_AUTHOR_EMAIL='41898282+github-actions[bot]@users.noreply.github.com'
  export GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
fi

status=0
result="$(GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0="http.$server/.extraheader" GIT_CONFIG_VALUE_0="$header" \
  target/debug/cargo-rail rail release --yes --format json "$@")" || status=$?
printf '%s\n' "$result"
state="$(jq -r '.state // empty' <<<"$result")"
tag="$(jq -r '.crates[0].tag // empty' <<<"$result")"
{
  printf 'state=%s\n' "$state"
  printf 'tag=%s\n' "$tag"
} >> "$GITHUB_OUTPUT"
exit "$status"
