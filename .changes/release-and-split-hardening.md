---
cargo-rail = "patch"
---

Make Git operations safer:

- When Cargo-Rail stages or cleans planned paths, it treats `*`, `?`,
  and `[` in file names as literal characters.
- A split `remote` must not start with `-`.
  Git receives every configured remote after `--`, so a value such as `--upload-pack=<command>` cannot run a command.
- The post-apply check for unplanned changes now sees files that appeared
  after Cargo-Rail captured the plan.
