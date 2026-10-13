---
cargo-rail = "patch"
---

On macOS, stage the compiler-observation wrappers as clones instead of hard links.
The first launch of a new executable waits for a Gatekeeper scan,
and a scan that resolved a hard link to another command's deleted copy terminated the launch.
