# Release - Segment 1: Introduction

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
Type "export PS1='~/testing/ripgrep> '"
Enter
Type "cd ~/loadingalias/testing/ripgrep"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Ripgrep workspace for release demo"
Enter
Sleep 1s

Type "cargo metadata --format-version 1 | jq '.workspace_members | length'"
Enter
Wait+Screen /\d+/
Sleep 2s

Type "# Current version"
Enter
Sleep 500ms
Type "grep '^version' Cargo.toml | head -1"
Enter
Sleep 3s
