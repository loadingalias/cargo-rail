#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <account-id> <bucket> <cache/cargo-rail/r2/v3/task5/run-prefix>" >&2
  exit 2
}

account="${1:-}"
bucket="${2:-}"
prefix="${3:-}"
[[ "$#" -eq 3 ]] || usage
[[ "$account" =~ ^[0-9a-f]{32}$ ]] || usage
[[ "$bucket" =~ ^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$ ]] || usage
[[ "$prefix" =~ ^cache/cargo-rail/r2/v3/task5/[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] || usage
[[ "$prefix" != */ && "$prefix" != *..* ]] || usage

for tool in aws jq; do
  command -v "$tool" >/dev/null || {
    echo "native-cache R2 cleanup requires $tool" >&2
    exit 2
  }
done

endpoint="https://$account.r2.cloudflarestorage.com"
authority="s3://$bucket/$prefix/"
aws s3 rm "$authority" --recursive --endpoint-url "$endpoint" --region auto --only-show-errors

while IFS=$'\t' read -r key upload_id; do
  [[ -n "$key" && -n "$upload_id" && "$key" == "$prefix/"* ]] || continue
  aws s3api abort-multipart-upload \
    --bucket "$bucket" \
    --key "$key" \
    --upload-id "$upload_id" \
    --endpoint-url "$endpoint" \
    --region auto
done < <(
  aws s3api list-multipart-uploads \
    --bucket "$bucket" \
    --prefix "$prefix/" \
    --endpoint-url "$endpoint" \
    --region auto \
    --output json \
    | jq -r '.Uploads[]? | [.Key, .UploadId] | @tsv'
)

objects="$(aws s3api list-objects-v2 \
  --bucket "$bucket" \
  --prefix "$prefix/" \
  --max-keys 1 \
  --endpoint-url "$endpoint" \
  --region auto \
  --query KeyCount \
  --output text)"
uploads="$(aws s3api list-multipart-uploads \
  --bucket "$bucket" \
  --prefix "$prefix/" \
  --max-uploads 1 \
  --endpoint-url "$endpoint" \
  --region auto \
  --output json \
  | jq '.Uploads // [] | length')"
[[ "$objects" == 0 && "$uploads" == 0 ]] || {
  echo "native-cache R2 cleanup left state under $authority" >&2
  exit 1
}

echo "native-cache R2 prefix absent: $authority"
