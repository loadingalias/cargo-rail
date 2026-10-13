---
cargo-rail = "patch"
---

Harden release recovery and Git operations:

- `release resume` refuses to adopt an existing local tag that points to a commit other than the release commit.
- Release records may not name paths inside the Git directory,
  so a fetched record cannot make recovery write `.git/config` or a hook.
- `cargo semver-checks`, `cargo package`, `cargo publish --dry-run`, and the MSRV check no longer receive `CARGO_REGISTRY_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, or other registry and forge tokens;
  these commands run build scripts and procedural macros.
- Staging and cleaning planned paths treats `*`, `?`, and `[` in file names literally.
- `clean` and release status refuse a release-state directory that links outside the workspace.
- A split `remote` may not start with `-`, and Git receives every configured remote after `--`,
  so a value such as `--upload-pack=<command>` cannot run a command.
- The post-apply check for unplanned changes now sees files that appeared
  after the plan was captured.
