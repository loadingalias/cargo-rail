#!/usr/bin/env bash
set -euo pipefail

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# GitHub Actions SHA Pinning Script
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#
# Purpose:
#   Resolves semantic versions from .github/actions-lock.yaml to commit SHAs,
#   then rewrites all workflow files to use SHA-pinned references.
#
# Usage:
#   ./scripts/ci/pin-actions.sh [--verify-only] [--update-lock]
#
# Options:
#   --verify-only    Check if workflows match lock file (CI mode)
#   --update-lock    Fetch latest SHAs and update lock file
#
# Requirements:
#   - yq (YAML processor): brew install yq OR apt-get install yq
#   - jq (JSON processor)
#   - gh (GitHub CLI) OR curl
#
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOCK_FILE="$REPO_ROOT/.github/actions-lock.yaml"
WORKFLOWS_DIR="$REPO_ROOT/.github/workflows"
ACTIONS_DIR="$REPO_ROOT/.github/actions"

VERIFY_ONLY=false
UPDATE_LOCK=false

# Parse arguments
for arg in "$@"; do
  case $arg in
    --verify-only)
      VERIFY_ONLY=true
      ;;
    --update-lock)
      UPDATE_LOCK=true
      ;;
    *)
      echo "Unknown option: $arg"
      echo "Usage: $0 [--verify-only] [--update-lock]"
      exit 1
      ;;
  esac
done

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Dependency Checks
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

check_dependencies() {
  local missing=()

  if ! command -v yq &> /dev/null; then
    missing+=("yq (install: brew install yq)")
  fi

  if ! command -v jq &> /dev/null; then
    missing+=("jq (install: brew install jq)")
  fi

  if ! command -v gh &> /dev/null && ! command -v curl &> /dev/null; then
    missing+=("gh or curl")
  fi

  if [ ${#missing[@]} -gt 0 ]; then
    echo "ERROR: Missing required dependencies:"
    printf '  - %s\n' "${missing[@]}"
    exit 1
  fi
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# GitHub API - Resolve ref to SHA
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

resolve_ref_to_sha() {
  local action="$1"  # e.g., "actions/checkout"
  local ref="$2"     # e.g., "v4" or "master"

  echo "  Resolving $action@$ref..." >&2

  # Try gh CLI first (respects auth, higher rate limits)
  if command -v gh &> /dev/null && gh auth status &> /dev/null; then
    local sha
    sha=$(gh api "repos/$action/commits/$ref" --jq '.sha' 2>/dev/null || echo "")
    if [ -n "$sha" ]; then
      echo "$sha"
      return 0
    fi
  fi

  # Fallback to curl (unauthenticated, 60 req/hour limit)
  local sha
  sha=$(curl -fsSL "https://api.github.com/repos/$action/commits/$ref" 2>/dev/null | jq -r '.sha // empty')

  if [ -z "$sha" ]; then
    echo "ERROR: Failed to resolve $action@$ref" >&2
    return 1
  fi

  echo "$sha"
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Update Lock File with Latest SHAs
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

update_lock_file() {
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "Updating actions-lock.yaml with latest SHAs"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  local actions
  actions=$(yq eval 'keys | .[]' "$LOCK_FILE" | grep -v '^#')

  local temp_lock
  temp_lock=$(mktemp)
  cp "$LOCK_FILE" "$temp_lock"
  local failed=false

  for action in $actions; do
    echo "Processing: $action"

    local ref
    ref=$(yq eval ".\"$action\".ref" "$LOCK_FILE")

    if [ "$ref" = "null" ] || [ -z "$ref" ]; then
      echo "  ❌ Missing ref" >&2
      failed=true
      continue
    fi

    local sha
    if ! sha=$(resolve_ref_to_sha "$action" "$ref"); then
      echo "  ❌ Failed to resolve SHA"
      failed=true
      continue
    fi

    local current_sha
    current_sha=$(yq eval ".\"$action\".sha" "$LOCK_FILE")
    if [ "$current_sha" != "$sha" ]; then
      local timestamp
      timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
      yq eval -i ".\"$action\".sha = \"$sha\"" "$temp_lock"
      yq eval -i ".\"$action\".updated = \"$timestamp\"" "$temp_lock"
    fi

    echo "  ✅ $ref → $sha"
    echo ""
  done

  if [ "$failed" = true ]; then
    rm -f "$temp_lock"
    echo "❌ Lock update aborted; no partial resolution was written." >&2
    return 1
  fi

  mv "$temp_lock" "$LOCK_FILE"
  echo "✅ Lock file updated successfully"
  echo ""
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Rewrite Workflow Files to Use SHA Pins
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

rewrite_workflows() {
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "Rewriting workflows to use SHA-pinned actions"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  # Find all YAML files in workflows/ and actions/
  local files
  files=$(find "$WORKFLOWS_DIR" "$ACTIONS_DIR" \( -name "*.yaml" -o -name "*.yml" \) -type f 2>/dev/null)

  for file in $files; do
    echo "Processing: $(basename "$file")"

    # Read all actions from lock file
    local actions
    actions=$(yq eval 'keys | .[]' "$LOCK_FILE" | grep -v '^#')

    local modified=false
    local temp_file
    temp_file=$(mktemp)
    cp "$file" "$temp_file"

    for action in $actions; do
      local ref sha
      ref=$(yq eval ".\"$action\".ref" "$LOCK_FILE")
      sha=$(yq eval ".\"$action\".sha" "$LOCK_FILE")

      if [ "$sha" = "null" ] || [ -z "$sha" ]; then
        echo "ERROR: $action has no SHA in $LOCK_FILE" >&2
        rm -f "$temp_file" "$temp_file.bak"
        return 1
      fi

      # Pattern: uses: actions/checkout@v4
      # Replace with: uses: actions/checkout@abc123...  # v4

      # Check if action exists in file
      if ! grep -q "uses: $action@" "$temp_file"; then
        continue
      fi

      # Perform replacement using sed
      # Match: uses: actions/checkout@<anything>
      # Replace: uses: actions/checkout@<sha>  # <ref>
      sed -i.bak -E "s|(uses: $action)@[a-zA-Z0-9._-]+( *#.*)?$|\1@$sha  # $ref|g" "$temp_file"

      modified=true
      echo "  ✅ Pinned $action@$ref → $sha"
    done

    if [ "$modified" = true ]; then
      mv "$temp_file" "$file"
      rm -f "$file.bak"
      echo "  💾 Saved changes"
    else
      rm -f "$temp_file" "$temp_file.bak"
      echo "  ℹ️  No changes needed"
    fi

    echo ""
  done

  echo "✅ All workflows updated"
  echo ""
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Verify Mode - Check if workflows are properly pinned
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

verify_workflows() {
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "Verifying workflows are properly pinned"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  local failed=false
  local used_actions
  used_actions=$(mktemp)

  while IFS= read -r file; do
    while IFS=: read -r line_number line; do
      local spec
      spec=$(sed -E 's/^[[:space:]]*uses:[[:space:]]*([^[:space:]#]+).*$/\1/' <<< "$line")
      if [[ "$spec" == ./* ]]; then
        continue
      fi
      if [[ "$spec" != *@* ]]; then
        echo "❌ INVALID: $file:$line_number: $line"
        failed=true
        continue
      fi

      local action sha
      action="${spec%@*}"
      sha="${spec##*@}"
      if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
        echo "❌ UNPINNED: $file:$line_number: $line"
        failed=true
        continue
      fi

      local locked_ref locked_sha
      locked_ref=$(yq eval ".\"$action\".ref" "$LOCK_FILE")
      locked_sha=$(yq eval ".\"$action\".sha" "$LOCK_FILE")
      if [ "$locked_ref" = "null" ] || [ "$locked_sha" = "null" ]; then
        echo "❌ UNLOCKED: $file:$line_number references $action"
        failed=true
        continue
      fi

      local comment_ref
      comment_ref=$(sed -nE 's/^.*#[[:space:]]*([^[:space:]]+)[[:space:]]*$/\1/p' <<< "$line")
      if [ "$sha" != "$locked_sha" ] || [ "$comment_ref" != "$locked_ref" ]; then
        echo "❌ LOCK DRIFT: $file:$line_number"
        echo "   workflow: $action@$sha  # ${comment_ref:-<missing>}"
        echo "   lock:     $action@$locked_sha  # $locked_ref"
        failed=true
      fi
      printf '%s\n' "$action" >> "$used_actions"
    done < <(grep -nE '^[[:space:]]*uses:' "$file" || true)
  done < <(find "$WORKFLOWS_DIR" "$ACTIONS_DIR" \( -name "*.yaml" -o -name "*.yml" \) -type f -print)

  local action
  while IFS= read -r action; do
    if ! grep -Fxq "$action" "$used_actions"; then
      echo "❌ STALE LOCK ENTRY: $action"
      failed=true
    fi
  done < <(yq eval 'keys | .[]' "$LOCK_FILE")
  rm -f "$used_actions"

  if [ "$failed" = true ]; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ VERIFICATION FAILED"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "To fix, run:"
    echo "  just pin-actions"
    echo ""
    exit 1
  else
    echo "✅ All workflows properly pinned"
    echo ""
  fi
}

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Main Execution
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

main() {
  check_dependencies

  if [ "$VERIFY_ONLY" = true ]; then
    verify_workflows
    exit 0
  fi

  if [ "$UPDATE_LOCK" = true ]; then
    update_lock_file
  fi

  rewrite_workflows
  verify_workflows

  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "✅ GitHub Actions pinning complete"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""
  echo "Next steps:"
  echo "  1. Review changes: git diff"
  echo "  2. Test workflows locally if possible"
  echo "  3. Commit changes: git add -A && git commit -m 'ci: pin GitHub Actions to commit SHAs'"
  echo ""
}

main
