# Change Detection - Segment 2: cargo rail affected

Output segments/2_affected.mp4

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
Type "export PS1='~/testing/tokio> '"
Enter
Type "cd ~/loadingalias/testing/tokio"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "# Which crates are affected by recent changes?"
Enter
Sleep 1s

Type "cargo rail affected --since HEAD~5"
Enter
Wait+Screen /changed files:|docs-only|test targets:|affected/
Sleep 4s

Type "# Different output formats for CI"
Enter
Sleep 1s

Type "cargo rail affected --since HEAD~5 -f names-only"
Enter
Wait+Screen /tokio|stream/
Sleep 3s

Type "# GitHub Actions matrix format"
Enter
Sleep 500ms
Type "cargo rail affected --since HEAD~5 -f github-matrix"
Enter
Wait+Screen /include|crate/
Sleep 4s
