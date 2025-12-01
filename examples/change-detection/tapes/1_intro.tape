# Change Detection - Segment 1: Introduction

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
Type "export PS1='~/testing/tokio> '"
Enter
Type "cd ~/loadingalias/testing/tokio"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Large workspace with many crates"
Enter
Sleep 1s

Type "cargo metadata --format-version 1 | jq '.workspace_members | length'"
Enter
Wait+Screen /\d+/
Sleep 2s

Type "# See recent changes"
Enter
Sleep 500ms
Type "git log --oneline -5"
Enter
Wait+Screen /[0-9a-f]{7}/
Sleep 3s
