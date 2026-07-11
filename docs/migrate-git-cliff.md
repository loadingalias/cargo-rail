# Migrate from git-cliff or release-plz

cargo-rail combines bump selection, graph-attributed changelogs, dependency-ordered publishing, tags, forge releases, and reviewed change files. It uses fixed changelog placeholders and groups instead of git-cliff's Tera templates.

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

## Custom Groups

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

git-cliff uses Tera templates. cargo-rail intentionally does not. Use the
fixed placeholder format instead:

| git-cliff value | cargo-rail placeholder |
| --- | --- |
| commit message body/summary | `{description}` |
| commit scope | `{scope}` |
| short commit id | `{sha}` |
| linked commit id | `{sha_link}` |
| pull request references | `{prs}` |
| conventional commit type | `{type}` |
| breaking marker | `{breaking}` |

Unsupported template logic should move to change files:

```bash
cargo rail change add rail-core --bump minor --message "Added graph-aware release planning."
```

## Paths

Do not migrate git-cliff monorepo path globs directly as the primary model.
cargo-rail attributes commits through the workspace graph:

1. changed file resolves to its owning crate,
2. a conventional-commit scope matching a crate name narrows attribution,
3. root infrastructure does not land in crate changelogs.

Use path filters only as an escape hatch:

```toml
[release.changelog.filters]
include_paths = ["crates/*/src/**"]
exclude_paths = ["crates/*/benches/**"]
```

Filters are authoritative: a commit scope can claim an otherwise
unattributed commit, but never one whose files the filters excluded.

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
cargo rail release run --all --bump auto --check
cargo rail release check --all --extended
```

Like release-plz, `--bump auto --all` only releases crates with
release-worthy changes; everything else is listed under `Skipped:` with the
reason and the tag range it was measured against.

`release check --extended` uses an installed `cargo-semver-checks` binary when
available. It is never added as a cargo-rail dependency, and an inconclusive
run (for example a first release with no published baseline) reports as
skipped — it never escalates a bump or fails the release.
