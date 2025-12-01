# jj - Segment 6: cargo rail unify (apply changes, normal speed)

Output segments/6_apply.mp4

Set Shell "bash"
Set FontSize 14
Set Width 1920
Set Height 1080
Set Theme "Catppuccin Mocha"
Set Padding 20
Set Framerate 60
Set TypingSpeed 30ms
Set WaitTimeout 5m

Hide
Type "export PS1='~/loadingalias/testing/jj> '"
Enter
Type "cd ~/loadingalias/testing/jj"
Enter
Sleep 500ms
Show

Sleep 500ms
Type "cargo rail unify"
Enter
Wait+Screen /next: cargo check/
Sleep 3s

Type "git diff --stat"
Enter
Wait+Screen /file(s)? changed/
Sleep 6s
