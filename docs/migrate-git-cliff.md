# Migrate from git-cliff or release-plz

Cargo-Rail combines bump selection, graph-attributed changelogs, dependency-ordered publishing, tags, forge releases,
and reviewed change files. It uses fixed placeholders and groups instead of git-cliff's Tera templates.

Start in commit-driven compatibility mode (`source = "commits"`) so conventional commits continue to select bumps and
prose. Move to reviewed changes after Cargo-Rail's plan matches the current release. Commit mode reconstructs intent
from history; changes mode records intent in `.changes/*.md` during review.

## Migration path

1. Configure `source = "commits"` and map the existing changelog policy.
2. Compare `cargo rail release check --all --bump auto` with the current release plan.
3. Add `.changes/*.md` files to new pull requests with `cargo rail change add`.
4. Switch to `source = "changes"` after every pending release has reviewed change intent.
5. Run `cargo rail change check --merge-base --required`, then remove the old release automation.

## git-cliff mapping

```toml
# cliff.toml
[changelog]
header = "# Changelog\n\n"
body = """
{% for group, commits in commits | group_by(attribute="group") %}
### {{ group }}
{% for commit in commits %}
- {{ commit.message }} ({{ commit.id | truncate(length=7) }})
{% endfor %}
{% endfor %}
"""

[git]
conventional_commits = true
filter_unconventional = true
commit_parsers = [
  { message = "^feat", group = "Features" },
  { message = "^fix", group = "Bug Fixes" },
  { message = "^perf", group = "Performance" },
  { message = "^docs", group = "Documentation" },
  { message = "^chore", skip = true },
]
```

Use:

```toml
[release]
# Keep commit-derived bumps and prose only during this compatibility migration.
source = "commits"
# git-cliff's filter_unconventional silently drops unparseable commits;
# fallback = "skip" below does the same. "deny" goes further and fails
# `release check` — use "warn" or "allow" to match git-cliff exactly.
unconventional_commits = "deny"

[release.changelog]
entry_format = "- {scope}{breaking}{description}{prs} ({sha_link})"
group_order = ["breaking", "feat", "fix", "perf", "docs", "other"]
fallback = "skip"

[release.changelog.filters]
skip_types = ["chore"]
skip_scopes = []
include_paths = []
exclude_paths = []
```

## Custom groups

git-cliff parser groups map to `[[release.changelog.groups]]` entries:

```toml
[[release.changelog.groups]]
types = ["sec", "security"]
title = "Security"
emoji = "🔒"

[[release.changelog.groups]]
types = ["deps"]
title = "Dependencies"
emoji = "📦"
```

Then put the type key in `group_order`:

```toml
[release.changelog]
group_order = ["breaking", "sec", "feat", "fix", "deps", "other"]
```

## Templates

Cargo-Rail does not run Tera templates. Use fixed placeholders:

| git-cliff value             | Cargo-Rail placeholder |
| --------------------------- | ---------------------- |
| commit message body/summary | `{description}`        |
| commit scope                | `{scope}`              |
| short commit id             | `{sha}`                |
| linked commit id            | `{sha_link}`           |
| pull request references     | `{prs}`                |
| conventional commit type    | `{type}`               |
| breaking marker             | `{breaking}`           |

Unsupported template logic should move to change files:

```bash
cargo rail change add rail-core --bump minor --message "Added graph-aware release planning."
```

## Paths

Do not make git-cliff path globs the primary model. Cargo-Rail attributes commits through the workspace graph:

1. changed file resolves to its owning crate,
2. a conventional-commit scope matching a crate name narrows attribution,
3. root infrastructure does not land in crate changelogs.

Use path filters only as an escape hatch:

```toml
[release.changelog.filters]
include_paths = ["crates/*/src/**"]
exclude_paths = ["crates/*/benches/**"]
```

Filters are authoritative. A commit scope can claim an unattributed commit, but not one excluded by path filters.

## release-plz mapping

release-plz's workspace changelog settings become workspace defaults:

```toml
[release.changelog]
path = "CHANGELOG.md"
relative_to = "crate"
```

Per-package changelog toggles become per-crate overrides:

```toml
[crates.internal-tool.changelog]
skip = true

[crates.public-api.changelog]
path = "HISTORY.md"
relative_to = "crate"
```

Use `--bump auto` for conventional-commit-driven bumps:

```bash
cargo rail release check --all --bump auto
cargo rail release check --all --extended
```

In commit mode, `--bump auto --all` releases only crates with release-worthy changes. Everything else is listed under
`Skipped:` with the reason and the tag range it was measured against.

`release check --extended` uses an installed `cargo-semver-checks` binary when available; Cargo-Rail does not depend
on it. An inconclusive run, such as a first release without a published baseline, reports as skipped. A confirmed
breaking change validates the selected bump and blocks an insufficient bump. It does not escalate the release.
