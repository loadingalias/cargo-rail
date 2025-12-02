# polars - Segment 6: cargo rail unify (apply changes, normal speed)

Output segments/6_apply.mp4

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
Type "export PS1='~/loadingalias/testing/polars> '"
Enter
Type "cd ~/loadingalias/testing/polars"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo rail unify"
Enter
Wait+Screen /next: cargo check|nothing to unify/
Sleep 3s

Type "git diff --stat"
Enter
Wait+Screen /file(s)? changed|~/loadingalias/testing/>/
Sleep 6s
