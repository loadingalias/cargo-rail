# Split & Sync - Segment 3: Verify split repo

Output segments/3_verify.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 40
Set Framerate 30
Set TypingSpeed 40ms
Set WaitTimeout 30s

Hide
Type "export PS1='/tmp/tokio-util-standalone> '"
Enter
Type "cd /tmp/tokio-util-standalone"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Verify history was preserved"
Enter
Sleep 500ms

Type "git log --oneline -5"
Enter
Wait+Screen /[0-9a-f]{7}/
Sleep 3s

Type "# Check file structure"
Enter
Sleep 500ms
Type "ls -la"
Enter
Sleep 3s
