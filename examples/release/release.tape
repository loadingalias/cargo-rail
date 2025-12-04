# cargo-rail release demo: ripgrep
# Version bump, changelog, and release preview

Output release.mp4

Set Shell "bash"
Set FontSize 16
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 50
Set Framerate 30
Set TypingSpeed 50ms

# Setup - reset repo, install cargo-rail (hidden)
Hide
Type "cd ~/loadingalias/testing/ripgrep && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml"
Enter
Sleep 2s
Type "cargo install cargo-rail --path ~/loadingalias/cargo-rail --force --quiet"
Enter
Sleep 5s
Type "clear"
Enter
Type "export PS1='ripgrep> '"
Enter
Sleep 500ms
Show

# --- 1. Show the repo ---
Sleep 1s
Type "# ripgrep - demonstrate release workflow"
Enter
Sleep 1s
Type "pwd && git log --oneline -1"
Enter
Sleep 3s

# --- 2. Show current version ---
Sleep 1s
Type "# Current version"
Enter
Sleep 1s
Type "clear && grep '^version' Cargo.toml | head -1"
Enter
Sleep 3s

# --- 3. cargo rail init ---
Sleep 1s
Type "# Initialize cargo-rail"
Enter
Sleep 1s
Type "clear && cargo rail init"
Enter
Sleep 3s

# --- 4. Configure release (show only [release] section) ---
Sleep 1s
Type "# Configure release options"
Enter
Sleep 1s
Type "cat >> .config/rail.toml << 'EOF'"
Enter
Type ""
Enter
Type "[release]"
Enter
Type 'tag_prefix = "v"'
Enter
Type 'tag_format = "{prefix}{version}"'
Enter
Type 'changelog_path = "CHANGELOG.md"'
Enter
Type "require_clean = true"
Enter
Type "EOF"
Enter
Sleep 1s
Type "grep -A10 '\\[release\\]' .config/rail.toml"
Enter
Sleep 3s

# --- 5. cargo rail release check ---
Sleep 1s
Type "# Check release readiness"
Enter
Sleep 1s
Type "clear && cargo rail release check ripgrep"
Enter
Sleep 5s

# --- 6. cargo rail release run --check (patch) ---
Sleep 1s
Type "# Preview patch release (dry run)"
Enter
Sleep 1s
Type "clear && cargo rail release run ripgrep --check"
Enter
Sleep 5s

# --- 7. cargo rail release run --bump minor --check ---
Sleep 1s
Type "# Preview minor version bump"
Enter
Sleep 1s
Type "clear && cargo rail release run ripgrep --bump minor --check"
Enter
Sleep 5s

# --- 8. cargo rail release run --bump explicit --check ---
Sleep 1s
Type "# Preview explicit version"
Enter
Sleep 1s
Type "clear && cargo rail release run ripgrep --bump 15.0.0 --check"
Enter
Sleep 5s

# --- Done ---
Sleep 1s
Type "# Done! Use without --check to execute the release."
Enter
Sleep 3s
