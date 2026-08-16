#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <bucket> <region> <exact Task 9 Cargo-Rail or sccache prefix>" >&2
  exit 2
}

bucket="${1:-}"
region="${2:-}"
prefix="${3:-}"
[[ "$#" -eq 3 ]] || usage
[[ "$bucket" =~ ^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$ ]] || usage
[[ "$region" =~ ^[a-z0-9-]+$ ]] || usage
if [[ ! "$prefix" =~ ^cache/cargo-rail/s3/v3/task9/[A-Za-z0-9][A-Za-z0-9._-]*$ \
  && ! "$prefix" =~ ^cache/cargo-rail/sccache/v3/aws/task9/[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  usage
fi
[[ "$prefix" != *..* ]] || usage

for tool in aws jq; do
  command -v "$tool" >/dev/null || {
    echo "native-cache S3 cleanup requires $tool" >&2
    exit 2
  }
done

authority="s3://$bucket/$prefix/"
aws s3 rm "$authority" --recursive --region "$region" >/dev/null

while :; do
  versions="$(aws s3api list-object-versions \
    --bucket "$bucket" \
    --prefix "$prefix/" \
    --max-items 1000 \
    --region "$region" \
    --output json)"
  delete="$(jq -c '{
    Objects: ([.Versions[]?, .DeleteMarkers[]?] | map({Key, VersionId})),
    Quiet: true
  }' <<<"$versions")"
  [[ "$(jq '.Objects | length' <<<"$delete")" -gt 0 ]] || break
  aws s3api delete-objects \
    --bucket "$bucket" \
    --delete "$delete" \
    --region "$region" >/dev/null
done

while IFS=$'\t' read -r key upload_id; do
  [[ -n "$key" && -n "$upload_id" ]] || continue
  aws s3api abort-multipart-upload \
    --bucket "$bucket" \
    --key "$key" \
    --upload-id "$upload_id" \
    --region "$region"
done < <(
  aws s3api list-multipart-uploads \
    --bucket "$bucket" \
    --prefix "$prefix/" \
    --region "$region" \
    --output json \
    | jq -r '.Uploads[]? | [.Key, .UploadId] | @tsv'
)

objects="$(aws s3api list-objects-v2 \
  --bucket "$bucket" \
  --prefix "$prefix/" \
  --max-keys 1 \
  --region "$region" \
  --query KeyCount \
  --output text)"
versions="$(aws s3api list-object-versions \
  --bucket "$bucket" \
  --prefix "$prefix/" \
  --max-items 1 \
  --region "$region" \
  --output json \
  | jq '[.Versions[]?, .DeleteMarkers[]?] | length')"
uploads="$(aws s3api list-multipart-uploads \
  --bucket "$bucket" \
  --prefix "$prefix/" \
  --max-uploads 1 \
  --region "$region" \
  --output json \
  | jq '.Uploads // [] | length')"
[[ "$objects" == 0 && "$versions" == 0 && "$uploads" == 0 ]] || {
  echo "native-cache S3 cleanup left state under $authority" >&2
  exit 1
}

echo "native-cache S3 prefix absent: $authority"
