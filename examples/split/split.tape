# cargo-rail split demo: tokio-util
# Extract a crate with preserved git history

Output split.mp4

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
Type "cd ~/loadingalias/testing/tokio && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml"
Enter
Sleep 2s
Type "rm -rf /tmp/tokio-util-standalone"
Enter
Sleep 1s
Type "cargo install cargo-rail --path ~/loadingalias/cargo-rail --force --quiet"
Enter
Sleep 5s
Type "clear"
Enter
Type "export PS1='tokio> '"
Enter
Sleep 500ms
Show

# --- 1. Show the repo ---
Sleep 1s
Type "# tokio monorepo - extract tokio-util with history"
Enter
Sleep 1s
Type "pwd && git log --oneline -1"
Enter
Sleep 3s

# --- 2. Show crate structure ---
Sleep 1s
Type "# Show crate to extract"
Enter
Sleep 1s
Type "clear && ls -la tokio-util/"
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

# --- 4. Configure split (show only [crates.*.split] section) ---
Sleep 1s
Type "# Configure split for tokio-util"
Enter
Sleep 1s
Type "cat >> .config/rail.toml << 'EOF'"
Enter
Type ""
Enter
Type "[crates.tokio-util.split]"
Enter
Type 'remote = "/tmp/tokio-util-standalone"'
Enter
Type 'branch = "main"'
Enter
Type 'mode = "single"'
Enter
Type 'paths = [{ crate = "tokio-util" }]'
Enter
Type 'include = ["LICENSE", "README.md"]'
Enter
Type "EOF"
Enter
Sleep 1s
Type "grep -A10 '\\[crates' .config/rail.toml"
Enter
Sleep 3s

# --- 5. cargo rail split --check ---
Sleep 1s
Type "# Preview split (dry run)"
Enter
Sleep 1s
Type "clear && cargo rail split run tokio-util --check"
Enter
Sleep 10s

# --- 6. cargo rail split run ---
Sleep 1s
Type "# Execute split with history preservation"
Enter
Sleep 1s
Type "clear && cargo rail split run tokio-util"
Enter
Sleep 30s

# --- 7. Verify the result ---
Sleep 1s
Type "# Verify: standalone repo created with history"
Enter
Sleep 1s
Type "clear && cd /tmp/tokio-util-standalone && git log --oneline | head -10"
Enter
Sleep 4s

Sleep 1s
Type "# Check file structure"
Enter
Sleep 1s
Type "ls -la"
Enter
Sleep 3s

# --- Done ---
Sleep 1s
Type "# Done! Crate extracted with deterministic SHAs."
Enter
Sleep 3s
