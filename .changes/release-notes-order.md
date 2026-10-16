---
cargo-rail = "patch"
---

List change entries in release notes and changelogs by bump level.
`major` entries come first, then `minor` entries, then `patch` entries.
Before, entries followed the change-file names.
Thus, a breaking change and its upgrade steps could come last.
Entries with the same level keep the order of their file names.
