# Release - Segment 2: cargo rail check

Output segments/2_check.mp4

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
Type "export PS1='~/testing/ripgrep> '"
Enter
Type "cd ~/loadingalias/testing/ripgrep"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Validate release readiness"
Enter
Sleep 500ms

Type "cargo rail check"
Enter
Wait+Screen /ready|warning|error|pass|fail|clean|dirty/
Sleep 4s

Type "# Check specific crate"
Enter
Sleep 500ms
Type "cargo rail check ripgrep"
Enter
Wait+Screen /ready|warning|error|pass|fail/
Sleep 4s
