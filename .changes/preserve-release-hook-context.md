---
"cargo-rail" = "patch"
---

Fixed release Git operations to preserve the caller environment for hooks, expose standard cargo-rail release context, and retain complete hook diagnostics. Removed the hook-bypassing push dry run while keeping one atomic branch-and-tag push.
