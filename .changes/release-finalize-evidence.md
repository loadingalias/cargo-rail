---
"cargo-rail" = "patch"
---

Finalizing a merged release pull request now tags the already-proven merge commit without creating or pushing an empty
commit or updating the protected branch. Recovery also recognizes transactions left by older versions that created a
legacy finalize commit.
