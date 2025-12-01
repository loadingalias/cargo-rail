# ripgrep - Segment 2: Baseline cargo check (will be sped up 4x)

Output segments/2_check.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 40
Set Framerate 30
Set TypingSpeed 40ms
Set WaitTimeout 10m

Hide
Type "export PS1='~/loadingalias/testing/ripgrep> '"
Enter
Type "cd ~/loadingalias/testing/ripgrep"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo check"
Enter
Wait+Screen /Finished|error\[/
Sleep 3s
