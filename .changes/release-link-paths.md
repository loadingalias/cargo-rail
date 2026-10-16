---
cargo-rail = "patch"
---

Refuse a release when the changelog path, the change directory, or a change file is a symbolic link.
Before, the release followed the link and wrote its target, which configuration does not name.
For example, `CHANGELOG.md -> target/CHANGELOG.md` committed the ignored `target/CHANGELOG.md`.
A link to a file outside the repository was already refused, but for a different reason.
`change status` and `change check` also refuse a linked change file or change directory.
`change add` refuses to write through a linked change directory.
