#!/usr/bin/env bash
set -euo pipefail

# release-plz wrapper for cargo-rail
# Usage:
#   ./scripts/release/release_plz.sh release-pr  # Create/update release PR
#   ./scripts/release/release_plz.sh release     # Publish (after PR merge)
#   ./scripts/release/release_plz.sh update      # Update changelog + versions locally

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

check_release_plz() {
  if ! command -v release-plz &> /dev/null; then
    echo "release-plz not found. Install with:"
    echo "  cargo install release-plz"
    exit 1
  fi
}

main() {
  check_release_plz
  cd "$REPO_ROOT"

  local cmd="${1:-release-pr}"
  shift || true

  case "$cmd" in
    release-pr)
      echo "Creating/updating release PR..."
      release-plz release-pr "$@"
      ;;
    release)
      echo "Publishing release..."
      release-plz release "$@"
      ;;
    update)
      echo "Updating changelog and versions locally..."
      release-plz update "$@"
      ;;
    *)
      echo "Usage: $0 {release-pr|release|update}"
      echo ""
      echo "Commands:"
      echo "  release-pr  Create or update a release PR on GitHub"
      echo "  release     Publish to crates.io (run after merging release PR)"
      echo "  update      Update changelog and versions locally (no PR/publish)"
      exit 1
      ;;
  esac
}

main "$@"
