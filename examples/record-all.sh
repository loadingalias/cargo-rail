#!/bin/bash
# Record all cargo-rail example demos
# Usage: ./examples/record-all.sh [repo1 repo2 ...]
# If no repos specified, records all

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CARGO_RAIL_ROOT="$(dirname "$SCRIPT_DIR")"

# All available repos
ALL_REPOS=(
    codex
    helix
    helix-db
    iced
    jj
    meilisearch
    polars
    ripgrep
    ruff
    tikv
    tokio
    vello
)

# Use specified repos or all
if [ $# -gt 0 ]; then
    REPOS=("$@")
else
    REPOS=("${ALL_REPOS[@]}")
fi

echo "=== cargo-rail demo recorder ==="
echo "Repos to record: ${REPOS[*]}"
echo ""

# Check dependencies
if ! command -v vhs &> /dev/null; then
    echo "ERROR: vhs not found. Install with: brew install vhs"
    exit 1
fi

if ! command -v ffmpeg &> /dev/null; then
    echo "ERROR: ffmpeg not found. Install with: brew install ffmpeg"
    exit 1
fi

# Install latest cargo-rail
echo "=== Installing cargo-rail ==="
cargo install --path "$CARGO_RAIL_ROOT" --force
echo "Using: $(which cargo-rail)"
cargo rail --version
echo ""

# Record each repo
FAILED=()
SUCCEEDED=()

for repo in "${REPOS[@]}"; do
    BUILD_SCRIPT="$SCRIPT_DIR/$repo/build.sh"

    if [ ! -f "$BUILD_SCRIPT" ]; then
        echo "SKIP: $repo (no build.sh found)"
        continue
    fi

    echo "=============================================="
    echo "=== Recording: $repo ==="
    echo "=============================================="

    if bash "$BUILD_SCRIPT"; then
        SUCCEEDED+=("$repo")
        echo "SUCCESS: $repo"
    else
        FAILED+=("$repo")
        echo "FAILED: $repo"
    fi

    # Give VHS/ttyd time to clean up between recordings
    # Kill any lingering VHS processes to prevent resource exhaustion
    pkill -9 ttyd 2>/dev/null || true
    pkill -9 chromium 2>/dev/null || true
    echo "Cooling down (3s)..."
    sleep 3
    echo ""
done

echo "=============================================="
echo "=== Summary ==="
echo "=============================================="
echo "Succeeded: ${#SUCCEEDED[@]}"
for repo in "${SUCCEEDED[@]}"; do
    MP4="$SCRIPT_DIR/$repo/demo.mp4"
    if [ -f "$MP4" ]; then
        SIZE=$(ls -lh "$MP4" | awk '{print $5}')
        echo "  ✓ $repo ($SIZE)"
    else
        echo "  ✓ $repo"
    fi
done

if [ ${#FAILED[@]} -gt 0 ]; then
    echo ""
    echo "Failed: ${#FAILED[@]}"
    for repo in "${FAILED[@]}"; do
        echo "  ✗ $repo"
    done
    exit 1
fi

echo ""
echo "All demos recorded! MP4 files are in examples/{repo}/demo.mp4"
