---
"cargo-rail" = "patch"
---

Fix GitHub release creation and recovery by retaining the release ID returned by GitHub and observing drafts by ID.
Recover a draft after a lost creation response through the paginated release list, and reject ambiguous matches
without creating another release.
