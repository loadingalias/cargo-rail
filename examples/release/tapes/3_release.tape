# Release - Segment 3: cargo rail release (dry run)

Output segments/3_release.mp4

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
Type "export PS1='~/testing/ripgrep> '"
Enter
Type "cd ~/loadingalias/testing/ripgrep"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Preview release plan (dry run)"
Enter
Sleep 500ms

Type "cargo rail release ripgrep --check"
Enter
Wait+Screen /would|version|changelog|tag|skip|bump|error|preview/
Sleep 5s

Type "# Different bump types"
Enter
Sleep 500ms
Type "cargo rail release ripgrep --bump minor --check"
Enter
Wait+Screen /would|version|minor|error/
Sleep 4s
