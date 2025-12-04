# cargo-rail unify demo: polars
# Large DataFrame library (33 crates) - shows dead feature pruning

Output polars.mp4

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
Type "cd ~/loadingalias/testing/polars && git reset --hard HEAD && git clean -fd && rm -rf .config/rail.toml"
Enter
Sleep 2s
Type "cargo install cargo-rail --path ~/loadingalias/cargo-rail --force --quiet"
Enter
Sleep 5s
Type "clear"
Enter
Type "export PS1='polars> '"
Enter
Sleep 500ms
Show

# --- 1. Show the repo ---
Sleep 1s
Type "# polars - high-performance DataFrame library (33 crates)"
Enter
Sleep 1s
Type "pwd && git log --oneline -1"
Enter
Sleep 3s

# --- 2. Build the repo (baseline) ---
Sleep 1s
Type "# Build baseline"
Enter
Sleep 1s
Type "clear && cargo build --workspace 2>&1 | tail -3"
Enter
Sleep 90s

# --- 3. cargo rail init ---
Sleep 1s
Type "# Initialize cargo-rail"
Enter
Sleep 1s
Type "clear && cargo rail init"
Enter
Sleep 3s

# --- 4. Configure unify (show only [unify] section) ---
Sleep 1s
Type "# Configure unify options"
Enter
Sleep 1s
Type "cat >> .config/rail.toml << 'EOF'"
Enter
Type ""
Enter
Type "[unify]"
Enter
Type "msrv = true"
Enter
Type "detect_unused = true"
Enter
Type "remove_unused = true"
Enter
Type "prune_dead_features = true"
Enter
Type "EOF"
Enter
Sleep 1s
Type "grep -A10 '\\[unify\\]' .config/rail.toml"
Enter
Sleep 3s

# --- 5. cargo rail unify --check ---
Sleep 1s
Type "# Preview changes (dry run)"
Enter
Sleep 1s
Type "clear && cargo rail unify --check"
Enter
Sleep 15s

# --- 6. cargo rail unify ---
Sleep 1s
Type "# Apply unification"
Enter
Sleep 1s
Type "clear && cargo rail unify"
Enter
Sleep 15s

# --- 7. cargo rail affected (show diff + stats) ---
Sleep 1s
Type "# Show what changed"
Enter
Sleep 1s
Type "clear && cargo rail affected --explain"
Enter
Sleep 5s

# --- 8. cargo build (verify non-breaking) ---
Sleep 1s
Type "# Verify build still works"
Enter
Sleep 1s
Type "clear && cargo build --workspace 2>&1 | tail -3"
Enter
Sleep 90s

# --- Done ---
Sleep 1s
Type "# Done! Dependencies unified, dead features pruned."
Enter
Sleep 3s
