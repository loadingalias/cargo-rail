# helix-db - Segment 2: Baseline cargo build (8x speedup)

Output segments/2_baseline.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 20
Set Framerate 60
Set TypingSpeed 30ms
Set WaitTimeout 10m

Hide
Type "cd ~/loadingalias/testing/helix-db"
Enter
Sleep 500ms
Show

Type "cargo build"
Enter
Wait+Screen /Finished|error\[/
Sleep 2s
