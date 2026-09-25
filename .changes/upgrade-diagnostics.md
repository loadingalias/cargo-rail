---
"cargo-rail" = "minor"
---

Upgrades now explain what earlier releases removed.
A removed command such as `cargo rail run` or `cargo rail release finalize` names the release that removed it and what to run instead.
A removed configuration key names its release and replacement in every command,
and `cargo rail config migrate` previews and applies its removal alongside default pruning.
Every unknown or removed key in a file is reported at once.
Planning a change that migrates such keys away no longer fails on the base commit's policy.
