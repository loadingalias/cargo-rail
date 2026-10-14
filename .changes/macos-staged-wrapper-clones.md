---
cargo-rail = "patch"
---

On macOS, stage the compiler-observation wrappers as clones instead of hard links.
The first launch of a new executable waits for a Gatekeeper scan.
When the scan resolved a hard link to the deleted copy of another command, it terminated the launch.
