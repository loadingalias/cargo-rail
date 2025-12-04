# cargo-rail sync demo: tokio-util
# Bidirectional sync between monorepo and split repo

Output sync.mp4

Set Shell "bash"
Set FontSize 16
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 50
Set Framerate 30
Set TypingSpeed 50ms

# Setup - reset repo, install cargo-rail, ensure split repo exists (hidden)
Hide
Type "cd ~/loadingalias/testing/tokio && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml"
Enter
Sleep 2s
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
Type "# tokio monorepo - bidirectional sync with split repo"
Enter
Sleep 1s
Type "pwd && git log --oneline -1"
Enter
Sleep 3s

# --- 2. cargo rail init ---
Sleep 1s
Type "# Initialize cargo-rail"
Enter
Sleep 1s
Type "clear && cargo rail init"
Enter
Sleep 3s

# --- 3. Configure sync (show only [crates.*.split] section) ---
Sleep 1s
Type "# Configure sync for tokio-util"
Enter
Sleep 1s
Type "cat >> .config/rail.toml << 'EOF'"
Enter
Type ""
Enter
Type "[crates.tokio-util.split]"
Enter
Type 'remote = "git@github.com:example/tokio-util.git"'
Enter
Type 'branch = "main"'
Enter
Type 'mode = "single"'
Enter
Type 'paths = [{ crate = "tokio-util" }]'
Enter
Type "EOF"
Enter
Sleep 1s
Type "grep -A10 '\\[crates' .config/rail.toml"
Enter
Sleep 3s

# --- 4. cargo rail sync --check ---
Sleep 1s
Type "# Check sync status (dry run)"
Enter
Sleep 1s
Type "clear && cargo rail sync tokio-util --check"
Enter
Sleep 5s

# --- 5. cargo rail sync --to-remote --check ---
Sleep 1s
Type "# Preview push to split repo"
Enter
Sleep 1s
Type "clear && cargo rail sync tokio-util --to-remote --check"
Enter
Sleep 5s

# --- 6. cargo rail sync --from-remote --check ---
Sleep 1s
Type "# Preview pull from split repo (creates branch)"
Enter
Sleep 1s
Type "clear && cargo rail sync tokio-util --from-remote --check"
Enter
Sleep 5s

# --- Done ---
Sleep 1s
Type "# Done! Use without --check to execute sync."
Enter
Sleep 3s
