# ripgrep - Segment 8: Verify cargo clean (will be sped up 2x)

Output segments/8_verify_clean.mp4

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
Type "export PS1='~/loadingalias/testing/ripgrep> '"
Enter
Type "cd ~/loadingalias/testing/ripgrep"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo clean"
Enter
Wait+Screen /Removed|Cleaning/
Sleep 2s
