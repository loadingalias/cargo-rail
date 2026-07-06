# Graph-Aware Changelog Engine

**Status**: Implemented
**Last Updated**: 2026-07-05
**Code Authority**: `src/release/changelog/`, `src/release/attribution.rs`, `src/release/change_files.rs`, `src/release/semver_checks.rs`, `src/release/planner.rs`, `src/release/version.rs`, `src/release/validator.rs`, `src/config/release.rs`, `src/change_detection/`, `src/graph/`

---

## Product Boundary

cargo-rail's release story is **planning, attribution, and verification** — not templating.

The differentiator is not "any changelog shape from any commit convention"
(git-cliff owns that at single-repo granularity). The differentiator is
**per-crate changelogs within one workspace, attributed by the dependency
graph, from one config and one invocation** — territory no existing tool
occupies:

- release-plz's `[changelog]` is workspace-global; per-package you can only
  toggle or redirect output, not reshape it.
- git-cliff's monorepo model is N config files + N invocations with path
  globs; the glob model silently drops tags (git-cliff #1122, #208), computes
  wrong per-package bumps (#648), and cannot route by scope (#749).
- No tool attributes commits to crates via the resolver. cargo-rail already
  owns that machinery (`change_detection` + `WorkspaceGraph`).

Rules:

1. Changelog shape is **declarative TOML**, using the same placeholder idiom
   as `tag_format`. No template engine. No Tera. The dependency-count story is
   a load-bearing trust signal; do not trade it for templating parity.
2. Attribution is resolver-backed, not glob-backed. Path filters remain only
   as an explicit escape hatch.
3. Workspace-level defaults, per-crate overrides. One config file. One run.
4. Intent files (changesets) take precedence over commit parsing when present.
5. External verifiers (cargo-semver-checks) are invoked as installed binaries,
   never vendored as dependencies — same policy as system git.
6. Deterministic output. Same inputs, byte-identical changelogs.

---

## Phase 1: Close the Spec'd Gaps

**Purpose**: Turn already-parsed conventional-commit data into automation, and
make `release check` diagnostics honest. Extends
`docs/tasks/planner-control-plane.md` Phase 3; supersedes its release-bump
items.

**Deliverables**:

- `--bump auto`:
  - per-crate semver inference from conventional commits since that crate's
    last matching tag (via `tag_format`),
  - mapping: breaking → major, feat → minor, fix/perf → patch, else no-op,
  - pre-1.0 policy knob: `[release] pre_1_breaking_bump = "major" | "minor"`
    (default `"minor"`; today's `BumpType::Major` 0.x→1.0.0 behavior stays for
    explicit `--bump major`),
  - crates with no qualifying commits are skipped, with a trace reason,
  - `release plan --bump auto` shows the inferred bump and the commits that
    caused it.
- Non-conventional commit diagnostics:
  - `release check` reports commits that failed conventional parsing in the
    release range, with near-miss detection (e.g. `hore(ci):` → "did you mean
    `chore(ci):`?"),
  - severity is configurable: `[release] unconventional_commits = "allow" |
    "warn" | "deny"` (default `"warn"`).
- cargo-semver-checks integration:
  - `release check --extended` runs `cargo semver-checks` per publishable
    library crate when the binary is installed; advisory by default,
  - `[release] semver_check = "off" | "warn" | "deny"` (default `"warn"`),
  - absent binary → single actionable notice, never a failure,
  - when semver-checks reports a breaking change that commit analysis missed,
    `--bump auto` escalates the bump and says why.
- Release-check taxonomy from planner-control-plane Phase 3 (missing
  changelog vs missing notes vs dirty worktree vs publish-disabled vs
  dependent-closure vs version-mismatch), each naming the exact crate.

**Tests**:

- `--bump auto` maps commit fixtures to expected bumps per crate, including
  pre-1.0 policy both ways
- tag_format variants resolve the correct "last matching tag" per crate
- typo'd commit types produce near-miss suggestions
- semver-checks absent → notice; present + breaking → bump escalation
- check taxonomy: one fixture per failure class, exact crate named

---

## Phase 2: Per-Crate Changelog Shape + Graph Attribution

**Purpose**: The differentiator. One declarative config defines changelog
shape at workspace level with per-crate overrides; commits reach the right
crate's changelog because the resolver says so, not because a glob matched.

### Config schema

Workspace defaults under `[release.changelog]`; every key overridable under
`[crates.NAME.changelog]` (which already carries `path` and `skip`):

```toml
[release.changelog]
# Entry rendering — same placeholder idiom as tag_format.
# Available: {scope} {description} {sha} {sha_link} {prs} {type} {breaking}
entry_format = "{scope}{description} {prs} ({sha_link})"
emoji = true
# Section order; unlisted parsed types fall through to `fallback`.
group_order = ["breaking", "feat", "fix", "perf", "docs"]
fallback = "other"        # "other" | "skip"

# Custom types and section overrides. Types not redefined here keep
# built-in titles/emoji.
[[release.changelog.groups]]
types = ["sec", "security"]
title = "Security"
emoji = "🔒"

[[release.changelog.groups]]
types = ["deps"]
title = "Dependencies"
emoji = "📦"

[release.changelog.filters]
skip_types = ["chore", "ci", "style"]
skip_scopes = []
# Escape hatch only; attribution is graph-based by default.
include_paths = []
exclude_paths = []
```

### Attribution semantics

For each commit in the release range:

1. Changed files → `change_detection` classification → owning crates via
   `WorkspaceGraph`. A cross-cutting commit lands in **every** owning crate's
   changelog. This is the fix for the git-cliff #1122/#648/#749 bug class.
2. Commit scope matching a workspace crate name **narrows** attribution to
   that crate (scope is an explicit human signal; files are the fallback
   truth). Scope not matching any crate → no effect on attribution.
3. Workspace-infra files (root manifests, CI, lockfile) attribute to no
   crate's changelog; they appear only in `release plan` trace output.
4. A crate bumped **only** through dependent-closure expansion gets a
   synthesized entry: `- updated {dependency} to {version}` under the
   Dependencies group — never an empty release section.

### Multi-format output

One generation pass renders:

- the crate's `CHANGELOG.md` section (existing behavior, now shaped by config),
- the GitHub release body (existing `release_notes_dir` override still wins),
- machine-readable JSON (commit, type, scope, crates attributed, bump
  contribution) via the existing `--format json` surface.

**Deliverables**:

- config schema above, validated in `ConfigError` with exact-field messages
- workspace→crate config merge (crate keys override, absent keys inherit)
- graph attribution replacing bare `git log -- <paths>` filtering
- synthesized dependency-bump entries
- JSON changelog output wired into `release plan`
- forge-neutral link rendering: `[release.changelog] commit_url` /
  `pr_url` templates (defaults inferred from GitHub remote as today)
- migration notes: `docs/migrate-git-cliff.md` mapping common `cliff.toml`
  parsers/groups to `[release.changelog]`; `release init` detects existing
  `cliff.toml` / `release-plz.toml` and points at the guide

**Tests**:

- golden changelog fixtures: default config byte-identical to current output
  (no silent format break for existing users)
- golden fixtures for custom groups, ordering, emoji off, entry_format
- TestWorkspace: commit touching crates A+B appears in both changelogs;
  commit touching only A never leaks into B
- scope narrowing: `fix(foo): ...` touching shared files lands only in foo
- dependent-closure crate gets synthesized entry, not empty section
- JSON output deterministic and schema-validated

---

## Phase 3: Change Files (Intent-Based Releases)

**Purpose**: Commit logs are engineer-facing; changelogs are user-facing.
High-churn workspaces — especially agent-authored commits per
`docs/operator-guide-ai-era.md` — need reviewed intent, not parsed syntax.
The JS ecosystem settled this (changesets over semantic-release); Rust has no
incumbent (knope is multi-ecosystem and small; cargo-changeset is abandoned-
scale). This space is winnable.

### Model

- Intent files live in `.rail/changes/*.md`, committed alongside the PR:

  ```markdown
  ---
  "rail-core" = "minor"
  "rail-cli" = "patch"
  ---

  Added `--bump auto`: version bumps are now inferred per crate from
  conventional commits since the last release tag.
  ```

- TOML frontmatter maps crate → bump; body is the user-facing changelog
  entry, written for release readers, one entry per file.
- Precedence: change files are authoritative for the crates they name.
  Conventional-commit analysis remains the fallback for crates a release
  touches that no change file covers (hybrid, not either/or).
- Files are consumed (deleted in the release commit) by `release run`.

**Deliverables**:

- `cargo rail change add [CRATE...]` — interactive/flagged scaffold; `--bump`
  and `--message` for non-interactive use
- `cargo rail change status` — pending intents, crates covered, inferred
  bumps
- parser + validation (unknown crate names, invalid bump values → exact-field
  errors)
- `--bump auto` consumes change-file bumps first, commit analysis fallback
- changelog generation renders change-file bodies as top-of-section entries,
  commit-derived entries after
- coverage gate: `[release] require_change_files = false | true |
  ["crate-a", "crate-b"]` — when set, `release check` fails if the release
  range touches a listed crate's code without a covering change file
- CI recipe docs: enforcing the gate on PRs (plan-only, no publish)

**Tests**:

- frontmatter parsing: valid, unknown crate, bad bump, empty body
- precedence: change file overrides commit-derived bump for named crates only
- hybrid: uncovered crate falls back to commit analysis in the same release
- consumption: files deleted in release commit; dry-run leaves them intact
- coverage gate: touch-without-intent fails only for configured crates
- golden output: change-file entries render above commit-derived entries

---

## Explicit Non-Goals

- A template engine (Tera or otherwise). Placeholder interpolation only.
- Per-repo config proliferation (the N-cliff.toml model).
- Replacing forge APIs beyond the existing `gh` usage.
- Commit-message linting as a standalone feature (diagnostics only).
- Further investment in publish ordering/atomicity as a differentiator —
  Cargo 1.90 native `publish --workspace` closed that gap.
- Changelog backfill/rewrite of historical releases.

---

## Relationship To Other Tasks

- Extends `docs/tasks/planner-control-plane.md` Phase 3 (release friction);
  the `--bump auto` and diagnostics items there are delivered by Phase 1 here.
- `docs/tasks/typescript.md` Phase 7 (unified release planning) inherits this
  engine: npm packages become attribution targets like crates. Nothing here
  may assume Rust-only crate identity in the config schema (crate names are
  strings, not validated against Cargo metadata at parse time).
- Marketing artifacts (`devto.md`, `docs/distribution/*`) currently claim
  "replaces git-cliff"; Phase 2 makes the claim defensible. Do not push the
  next launch cycle before Phase 2 lands or soften the claim.

---

## Acceptance Condition

This task is complete when:

1. `--bump auto` infers correct per-crate bumps from commits and change
   files, with semver-checks escalation when installed.
2. A workspace defines changelog shape once, overrides per crate, and a
   cross-cutting commit lands correctly in every affected crate's changelog —
   demonstrated by an integration test no glob-based tool can pass.
3. Default output is byte-identical to today's for unconfigured users.
4. Change files round-trip: scaffold → gate → render → consume.
5. `docs/migrate-git-cliff.md` maps a real-world `cliff.toml` to
   `[release.changelog]` without loss for non-template features.
6. The dependency tree gains zero new crates for templating; semver-checks
   remains an external binary.
