# helix-db - Segment 3: cargo rail init (normal speed)

Output segments/3_init.mp4

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
Type "export PS1='~/loadingalias/testing/helix-db> '"
Enter
Type "cd ~/loadingalias/testing/helix-db"
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
