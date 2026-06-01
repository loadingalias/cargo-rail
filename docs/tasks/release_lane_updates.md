# Release Lane

Status: active. Owner: @loadingalias.

cargo-rail is the release tool. If it publishes to crates.io and then asks the
operator to push tags or hand-create a GitHub Release, it is not owning the
release. The release lane is modeled as one state machine with provider
backends, not as a crates.io script plus forge afterthoughts.

The intended operator command is:

```bash
cargo rail release run --all --bump patch --yes
```

For an end-to-end GitHub release:

```toml
[release]
push = true
create_github_release = true
sign_tags = true
```

`create_github_release = true` without `push = true` is invalid. A GitHub
Release must target a tag that cargo-rail has already pushed; letting GitHub
invent a tag from the default branch is unsafe.

## State Machine

1. Preflight before mutation:
   - clean tree when `require_clean = true`
- selected crates exist and are publishable
- publishable target versions do not appear on crates.io
   - changelog paths stay inside the workspace
   - target local and remote tags do not conflict
   - `origin` exists when `push = true`
   - branch can be pushed with `git push --dry-run origin HEAD:<branch>`
   - `gh` exists and is authenticated when GitHub releases are enabled
   - release notes exist or can be generated under the GitHub body limit

2. Apply local release changes:
   - bump crate versions
   - update dependent versions
   - update lockfiles
   - generate changelog or use `release-notes/v<version>.md`
   - commit the release bump
   - create annotated or signed tags

3. Push before public release:
   - `git push --atomic origin HEAD:<branch> refs/tags/<tag>...`
   - if the push fails, stop before registry publish

4. Create GitHub draft releases:
   - target the exact pushed commit with `--target`
   - use generated or manual release notes
   - keep releases draft while registry publication runs

5. Publish registries:
   - Rust today: crates.io
   - future providers: npm, PyPI, Docker, Homebrew

6. Publish GitHub releases:
   - convert drafts to public
   - mark latest for now
   - fail hard if publication fails

7. Be explicit on rerun:
   - existing releases are reported
   - conflicting remote tags are hard errors
   - no silent best-effort release behavior

## Implemented Now

- `[release].push` owns release commit and tag push.
- `[release].create_github_release` requires `push = true`.
- Release refs are pushed atomically before any crates.io publish.
- GitHub Releases are created as drafts before registry publish and published
  after registry publish.
- GitHub Releases use `--target <HEAD>` so the release cannot drift to the
  default branch.
- GitHub release creation and publication fail hard when enabled.
- crates.io target versions are checked before mutation using Cargo registry
  metadata.
- Release notes are extracted from the new changelog section, not the whole
  changelog.
- Manual release notes override generated notes via
  `release-notes/v<version>.md` or `release-notes/<tag>.md`.
- GitHub release notes are checked against a 120 KB safety limit before commit,
  tag, push, or publish.

## Provider Architecture

The release graph should stay generic:

- version backend: `Cargo.toml`, later `package.json`, etc.
- registry publisher: crates.io, later npm, PyPI, Docker, Homebrew
- VCS backend: git
- forge backend: GitHub Releases now, GitLab later
- changelog backend: conventional commits now, module-scoped and custom
  patterns next

`cargo-rail-action` must stay thin: install cargo-rail, provide credentials and
permissions, run the same command. The action must not contain a second release
implementation.

## Remaining Work

- Add configurable changelog parsing under `[release.changelog]`:
  conventional commits, module-scoped commits, custom named-capture regexes,
  ordered section rules, entry templates, and multiline body splitting.
- Replace the initial crates.io existence check with a proper provider API when
  the registry provider abstraction lands, including yanked historical versions.
- Add richer idempotence receipts so reruns can resume exact partial states:
  local commit exists, local tag exists, remote tag exists, draft release exists,
  crate already published, release already public.
- Add configurable GitHub `latest` behavior.
- Extend the provider interface before adding non-Rust registry publishers.

## Release Criteria

The cargo-rail release that ships this lane should itself use the new flow:

```bash
cargo rail release run cargo-rail --bump patch --yes
```

Use a manual `release-notes/v<version>.md` when the first generated section is
too large or when the release needs curated narrative notes.
