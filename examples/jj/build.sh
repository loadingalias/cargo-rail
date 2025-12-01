#!/bin/bash
# Build demo.mp4 for jj
# Records all segments, speeds up cargo check 8x, concatenates

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Clean up test repo first
TEST_REPO="$HOME/loadingalias/testing/jj"
if [ -d "$TEST_REPO" ]; then
    echo "=== Resetting test repo ==="
    (cd "$TEST_REPO" && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml) 2>/dev/null || true
fi

mkdir -p segments

echo "=== Recording segments ==="
for tape in tapes/*.tape; do
    name=$(basename "$tape" .tape)
    echo "Recording: $name"
    vhs "$tape"
done

echo "=== Speeding up cargo segments (8x) ==="
# 8x speedup: setpts=0.125*PTS (1/8 = 0.125)
ffmpeg -y -i segments/2_baseline.mp4 -vf "setpts=0.125*PTS" -an segments/2_baseline_fast.mp4
ffmpeg -y -i segments/7_verify.mp4 -vf "setpts=0.125*PTS" -an segments/7_verify_fast.mp4

echo "=== Concatenating segments ==="
cat > segments/concat.txt << 'EOF'
file '1_intro.mp4'
file '2_baseline_fast.mp4'
file '4_init.mp4'
file '5_preview.mp4'
file '6_apply.mp4'
file '7_verify_fast.mp4'
EOF

ffmpeg -y -f concat -safe 0 -i segments/concat.txt -c copy demo.mp4

echo "=== Done ==="
ls -lh demo.mp4
echo ""
echo "Output: $SCRIPT_DIR/demo.mp4"
