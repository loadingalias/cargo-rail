# Split & Sync - Segment 4: cargo rail sync

Output segments/4_sync.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 40
Set Framerate 30
Set TypingSpeed 40ms
Set WaitTimeout 2m

Hide
Type "export PS1='~/testing/tokio> '"
Enter
Type "cd ~/loadingalias/testing/tokio"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Check sync status"
Enter
Sleep 500ms

Type "cargo rail sync tokio-util --check"
Enter
Wait+Screen /sync|up to date|changes|ahead|behind|error/
Sleep 4s

Type "# Sync strategies: ours, theirs, manual, union"
Enter
Sleep 2s
Type "cargo rail sync --help | grep -A2 'strategy'"
Enter
Sleep 4s
