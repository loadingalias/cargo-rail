# vello - Segment 3: Initialize cargo-rail

Output segments/3_init.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 20
Set Framerate 60
Set TypingSpeed 30ms
Set WaitTimeout 1m

Hide
Type "export PS1='~/loadingalias/testing/vello> '"
Enter
Type "cd ~/loadingalias/testing/vello"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo rail --version"
Enter
Sleep 2s

Type "cargo rail init --non-interactive"
Enter
Wait+Screen /next:|created:/
Sleep 4s
