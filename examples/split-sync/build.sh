#!/bin/bash
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

TEST_REPO="$HOME/loadingalias/testing/tokio"
if [ -d "$TEST_REPO" ]; then
    echo "=== Resetting test repo ==="
    (cd "$TEST_REPO" && git checkout -- . && git clean -fd) 2>/dev/null || true
fi

# Clean up any previous split
rm -rf /tmp/tokio-util-standalone

mkdir -p segments

echo "=== Recording segments ==="
for tape in tapes/*.tape; do
    echo "Recording: $(basename "$tape" .tape)"
    vhs "$tape"
done

echo "=== Concatenating segments ==="
cat > segments/concat.txt << 'EOF'
file '1_intro.mp4'
file '2_split.mp4'
file '3_verify.mp4'
file '4_sync.mp4'
EOF

ffmpeg -y -f concat -safe 0 -i segments/concat.txt -c copy demo.mp4

echo "=== Done ==="
ls -lh demo.mp4
