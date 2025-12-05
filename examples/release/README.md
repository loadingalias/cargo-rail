# release demo

Demonstrates release validation and planning using tikv (70+ component workspace).

## Workflow

```bash
cargo rail init                              # Base config
cargo rail release init tikv                 # Configure crate for release
cargo rail release check tikv                # Basic validation
cargo rail release check tikv --extended     # Full validation (dry-run, MSRV)
cargo rail release run tikv --check          # Preview release plan
cargo rail release run tikv --bump minor     # Execute minor release
```

## Config

```toml
[release]
tag_prefix = "v"
tag_format = "{crate}-{prefix}{version}"
require_clean = true
publish_delay = 5
create_github_release = false
require_changelog_entries = false

[crates.tikv.release]
publish = false
```

| Option | Value | Why |
|--------|-------|-----|
| `tag_format` | `{crate}-{prefix}{version}` | Produces `tikv-v9.0.1` style tags |
| `require_clean` | `true` | Enforce clean git state |
| `create_github_release` | `false` | Skip gh CLI for demo |
| `publish` | `false` | tikv isn't published to crates.io |

## What this shows

- `cargo rail release init` - Configure a crate for release
- `cargo rail release check` - Basic validation
- `cargo rail release check --extended` - Full dry-run + MSRV check
- `cargo rail release run --check` - Preview release plan
- `--bump patch/minor/major` - Version bump options
- `--skip-publish` - Tag-only releases
