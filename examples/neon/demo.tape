# neon - cargo-rail demo
# Uses hakari - needs pin_transitives=true and include_renamed=true

Output examples/neon/demo.gif

Set FontSize 14
Set Width 900
Set Height 600
Set Theme "Catppuccin Mocha"
Set Padding 20
Set TypingSpeed 75ms

Type "cd ~/loadingalias/testing/neon"
Enter
Sleep 1s

Type "cargo rail init"
Enter
Sleep 3s

Type "sed -i '' 's/pin_transitives = false/pin_transitives = true/' .config/rail.toml"
Enter
Sleep 500ms

Type "sed -i '' 's/include_renamed = false/include_renamed = true/' .config/rail.toml"
Enter
Sleep 500ms

Type "cargo rail unify --check"
Enter
Sleep 5s

Type "cargo rail unify"
Enter
Sleep 4s

Type "cargo rail affected --since HEAD~5"
Enter
Sleep 4s

Sleep 2s
