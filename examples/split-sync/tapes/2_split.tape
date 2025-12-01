# Split & Sync - Segment 2: cargo rail split

Output segments/2_split.mp4

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
Type "export PS1='~/testing/tokio> '"
Enter
Type "cd ~/loadingalias/testing/tokio"
Enter
Type "rm -rf /tmp/tokio-util-standalone"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Preview split (dry run)"
Enter
Sleep 500ms

Type "cargo rail split tokio-util --check"
Enter
Wait+Screen /would|check|preview|files|commits|error/
Sleep 4s

Type "# Execute the split with history"
Enter
Sleep 500ms

Type "cargo rail split tokio-util"
Enter
Wait+Screen /Created|complete|success|done|error/
Sleep 4s
