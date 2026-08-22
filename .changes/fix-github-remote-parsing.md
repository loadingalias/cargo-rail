---
"cargo-rail" = "patch"
---

Fixed GitHub repository detection for remote URLs with a trailing slash, and rejected non-repository paths before
they could produce incorrect changelog or release links. Release transactions now bind the one effective origin
repository shared by fetch and push operations, persist it for recovery, and target forge commands explicitly.
