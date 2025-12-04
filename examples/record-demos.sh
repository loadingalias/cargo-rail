#!/bin/bash
# Record cargo-rail demo videos
#
# Usage:
#   ./examples/record-demos.sh           # Record all demos
#   ./examples/record-demos.sh unify     # Record only unify demos
#   ./examples/record-demos.sh split     # Record only split demo
#   ./examples/record-demos.sh release   # Record only release demo
#   ./examples/record-demos.sh sync      # Record only sync demo

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CARGO_RAIL_ROOT="$(dirname "$SCRIPT_DIR")"

# Check dependencies
check_deps() {
    local missing=()
    command -v vhs &>/dev/null || missing+=("vhs (brew install vhs)")
    command -v ffmpeg &>/dev/null || missing+=("ffmpeg (brew install ffmpeg)")

    if [ ${#missing[@]} -gt 0 ]; then
        echo "Missing dependencies:"
        for dep in "${missing[@]}"; do
            echo "  - $dep"
        done
        exit 1
    fi
}

# Install latest cargo-rail
install_cargo_rail() {
    echo "=== Installing cargo-rail ==="
    cargo install --path "$CARGO_RAIL_ROOT" --force --quiet
    echo "Using: $(cargo rail --version)"
    echo ""
}

# Reset a test repository
reset_repo() {
    local repo_path="$1"
    if [ -d "$repo_path" ]; then
        echo "Resetting: $repo_path"
        (cd "$repo_path" && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml) 2>/dev/null || true
    fi
}

# Record a single tape and optionally speed up cargo commands
record_tape() {
    local tape="$1"
    local output_dir="$2"
    local output_name="$3"

    echo "Recording: $tape -> $output_dir/$output_name"

    mkdir -p "$output_dir"
    cd "$output_dir"

    # Record
    vhs "$tape"

    # Get the output filename from the tape
    local raw_output
    raw_output=$(grep "^Output " "$tape" | head -1 | cut -d' ' -f2)

    # Speed up cargo check sections (2x) for final output
    # For now, just rename if needed
    if [ -f "$raw_output" ] && [ "$raw_output" != "$output_name" ]; then
        mv "$raw_output" "$output_name"
    fi

    local size
    size=$(ls -lh "$output_name" 2>/dev/null | awk '{print $5}')
    echo "Created: $output_name ($size)"
}

# Record unify demos
record_unify() {
    echo "=============================================="
    echo "=== Recording Unify Demos ==="
    echo "=============================================="

    local unify_dir="$SCRIPT_DIR/unify"
    mkdir -p "$unify_dir"

    for repo in polars tokio ripgrep; do
        reset_repo "$HOME/loadingalias/testing/$repo"

        echo ""
        echo "--- Recording: $repo ---"

        cd "$unify_dir"
        vhs "$unify_dir/$repo.tape"

        local size
        size=$(ls -lh "$unify_dir/$repo.mp4" 2>/dev/null | awk '{print $5}')
        echo "Created: $repo.mp4 ($size)"

        # Kill lingering processes
        pkill -9 ttyd 2>/dev/null || true
        sleep 2
    done
}

# Record split demo
record_split() {
    echo "=============================================="
    echo "=== Recording Split Demo ==="
    echo "=============================================="

    local split_dir="$SCRIPT_DIR/split"
    mkdir -p "$split_dir"

    reset_repo "$HOME/loadingalias/testing/tokio"
    rm -rf /tmp/tokio-util-standalone

    cd "$split_dir"
    vhs "$split_dir/split.tape"

    local size
    size=$(ls -lh "$split_dir/split.mp4" 2>/dev/null | awk '{print $5}')
    echo "Created: split.mp4 ($size)"

    pkill -9 ttyd 2>/dev/null || true
    sleep 2
}

# Record release demo
record_release() {
    echo "=============================================="
    echo "=== Recording Release Demo ==="
    echo "=============================================="

    local release_dir="$SCRIPT_DIR/release"
    mkdir -p "$release_dir"

    reset_repo "$HOME/loadingalias/testing/ripgrep"

    cd "$release_dir"
    vhs "$release_dir/release.tape"

    local size
    size=$(ls -lh "$release_dir/release.mp4" 2>/dev/null | awk '{print $5}')
    echo "Created: release.mp4 ($size)"

    pkill -9 ttyd 2>/dev/null || true
    sleep 2
}

# Record sync demo
record_sync() {
    echo "=============================================="
    echo "=== Recording Sync Demo ==="
    echo "=============================================="

    local sync_dir="$SCRIPT_DIR/sync"
    mkdir -p "$sync_dir"

    reset_repo "$HOME/loadingalias/testing/tokio"

    cd "$sync_dir"
    vhs "$sync_dir/sync.tape"

    local size
    size=$(ls -lh "$sync_dir/sync.mp4" 2>/dev/null | awk '{print $5}')
    echo "Created: sync.mp4 ($size)"

    pkill -9 ttyd 2>/dev/null || true
    sleep 2
}

# Summary of all demos
print_summary() {
    echo ""
    echo "=============================================="
    echo "=== Demo Summary ==="
    echo "=============================================="

    for mp4 in "$SCRIPT_DIR"/unify/*.mp4 "$SCRIPT_DIR"/split/*.mp4 "$SCRIPT_DIR"/sync/*.mp4 "$SCRIPT_DIR"/release/*.mp4; do
        if [ -f "$mp4" ]; then
            local name size
            name=$(basename "$mp4")
            size=$(ls -lh "$mp4" | awk '{print $5}')
            echo "  $name ($size)"
        fi
    done
}

# Main
main() {
    echo "=== cargo-rail demo recorder ==="
    echo ""

    check_deps
    install_cargo_rail

    local target="${1:-all}"

    case "$target" in
        unify)
            record_unify
            ;;
        split)
            record_split
            ;;
        release)
            record_release
            ;;
        sync)
            record_sync
            ;;
        all)
            record_unify
            record_split
            record_sync
            record_release
            ;;
        *)
            echo "Unknown target: $target"
            echo "Usage: $0 [unify|split|sync|release|all]"
            exit 1
            ;;
    esac

    print_summary

    echo ""
    echo "Done! Demo videos are in examples/{unify,split,sync,release}/"
}

main "$@"
