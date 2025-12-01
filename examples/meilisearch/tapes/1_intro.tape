# meilisearch - Segment 1: Introduction

Output segments/1_intro.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 20
Set Framerate 60
Set TypingSpeed 30ms
Set WaitTimeout 30s

Hide
Type "export PS1='~/loadingalias/testing/meilisearch> '"
Enter
Type "cd ~/loadingalias/testing/meilisearch"
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
