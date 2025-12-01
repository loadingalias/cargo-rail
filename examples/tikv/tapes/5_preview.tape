# tikv - Segment 5: cargo rail unify --check (longer pause for viewer)

Output segments/5_preview.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 40
Set Framerate 30
Set TypingSpeed 40ms
Set WaitTimeout 5m

Hide
Type "export PS1='~/loadingalias/testing/tikv> '"
Enter
Type "cd ~/loadingalias/testing/tikv"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo rail unify --check"
Enter
Wait+Screen /Summary|No changes|dependencies|error\[/
Sleep 7s
