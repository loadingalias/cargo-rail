---
"cargo-rail" = "patch"
---

Cargo-Rail now finds its release components beside the real executable
when it runs through a symlink.
Before, on macOS, `cargo rail cache setup` through a launcher symlink on `PATH` reported the compiler worker as unavailable
and asked for a reinstall.
