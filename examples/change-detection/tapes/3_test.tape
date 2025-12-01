# Change Detection - Segment 3: cargo rail test (will be sped up)

Output segments/3_test.mp4

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
Type "export PS1='~/testing/tokio> '"
Enter
Type "cd ~/loadingalias/testing/tokio"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Test only affected crates with --explain"
Enter
Sleep 1s

Type "cargo rail test --since HEAD~5 --explain"
Enter
Wait+Screen /test result:|Finished|PASSED|FAILED|error\[|running/
Sleep 4s
