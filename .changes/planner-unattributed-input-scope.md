---
"cargo-rail" = "patch"
---

Keep every Cargo unit in scope when one change edits both a target root and a non-Rust file
that no evidence attributes: the unattributed file now widens Cargo work to the workspace instead of
only the edited package.
Packaging work also selects each workspace member whose directory holds a changed file.
