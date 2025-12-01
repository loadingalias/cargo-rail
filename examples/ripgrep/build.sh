#!/bin/bash
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"
REPO_NAME="ripgrep"

TEST_REPO="$HOME/loadingalias/testing/$REPO_NAME"
if [ -d "$TEST_REPO" ]; then
    echo "=== Resetting test repo ==="
    (cd "$TEST_REPO" && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml) 2>/dev/null || true
fi

mkdir -p segments

echo "=== Recording segments ==="
for tape in tapes/*.tape; do
    echo "Recording: $(basename "$tape" .tape)"
    vhs "$tape"
done

echo "=== Speeding up cargo segments (2x) ==="
# Before unify
ffmpeg -y -i segments/2_check.mp4 -vf "setpts=0.5*PTS" -an segments/2_check_fast.mp4
ffmpeg -y -i segments/3_build.mp4 -vf "setpts=0.5*PTS" -an segments/3_build_fast.mp4
# After unify (verification: clean, check, build)
ffmpeg -y -i segments/8_verify_clean.mp4 -vf "setpts=0.5*PTS" -an segments/8_verify_clean_fast.mp4
ffmpeg -y -i segments/9_verify_check.mp4 -vf "setpts=0.5*PTS" -an segments/9_verify_check_fast.mp4
ffmpeg -y -i segments/10_verify_build.mp4 -vf "setpts=0.5*PTS" -an segments/10_verify_build_fast.mp4

echo "=== Concatenating segments ==="
cat > segments/concat.txt << 'EOF'
file '1_intro.mp4'
file '2_check_fast.mp4'
file '3_build_fast.mp4'
file '4_init.mp4'
file '5_preview.mp4'
file '6_apply.mp4'
file '8_verify_clean_fast.mp4'
file '9_verify_check_fast.mp4'
file '10_verify_build_fast.mp4'
EOF

ffmpeg -y -f concat -safe 0 -i segments/concat.txt -c copy demo.mp4

echo "=== Done ==="
ls -lh demo.mp4
