---
cargo-rail = "patch"
---

Reuse units whose procedural macros search upward past the workspace root for a missing file.
For example, `sqlx` 0.9 looks for a `.env` file in each directory up to the file-system root.
Before this change, each check outside the repository made the unit bypass with `procedural_macro_read_outside_repository`.
The action now binds the absence of that name in every directory above the root.
If a file with that name exists above the root, the unit still runs on the ordinary compiler path.
Remote storage neither publishes nor restores these results,
because directories outside the repository decide them.
