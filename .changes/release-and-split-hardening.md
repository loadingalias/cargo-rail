---
cargo-rail = "patch"
---

Harden Git operations:

- Staging and cleaning planned paths treats `*`, `?`, and `[` in file names literally.
- A split `remote` may not start with `-`, and Git receives every configured remote after `--`,
  so a value such as `--upload-pack=<command>` cannot run a command.
- The post-apply check for unplanned changes now sees files that appeared
  after the plan was captured.
