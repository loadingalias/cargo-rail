# codex - Segment 1: Introduction (show repo + commit)

Output segments/1_intro.mp4

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
Type "export PS1='~/loadingalias/testing/codex> '"
Enter
Type "cd ~/loadingalias/testing/codex/codex-rs"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "pwd"
Enter
Sleep 1s

Type "git log --oneline -1"
Enter
Wait+Screen /[0-9a-f]{7}/
Sleep 4s
