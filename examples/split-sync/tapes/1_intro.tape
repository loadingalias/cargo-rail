# Split & Sync - Segment 1: Introduction

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
Type "# Tokio workspace has many crates we can split out"
Enter
Sleep 1s

Type "ls -d */ | head -10"
Enter
Sleep 2s

Type "# Show workspace structure"
Enter
Sleep 500ms
Type "cargo metadata --format-version 1 | jq '.workspace_members | length'"
Enter
Wait+Screen /\d+/
Sleep 3s
