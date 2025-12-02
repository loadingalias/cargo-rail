# Command Reference

> Auto-generated from `cargo rail --help`. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`

---

## cargo rail

```
The rail subcommand

Usage: cargo rail [OPTIONS] <COMMAND>

Commands:
  affected  Show which crates are affected by changes
  test      Run tests for affected crates only
  unify     Unify workspace dependencies (replaces workspace-hack crates)
  init      Initialize configuration (rail.toml)
  split     Split a crate to a standalone repository with git history
  sync      Sync changes between monorepo and split repos
  release   Publish releases (version bump, changelog, tag, publish)
  check     Validate release readiness
  clean     Clean generated artifacts (cache, backups, reports)
  config    Configuration management
  help      Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet    Suppress progress messages (for CI/automation)
      --json     Output in JSON format (shorthand for -f json)
  -h, --help     Print help
  -V, --version  Print version
```

---

## cargo rail affected

```
Show which crates are affected by changes

Usage: cargo rail affected [OPTIONS]

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --since <SINCE>
          Git ref to compare against (auto-detects origin/main or origin/master)

      --from <FROM>
          Start ref (for SHA pair mode)

      --json
          Output in JSON format (shorthand for -f json)

      --to <TO>
          End ref (for SHA pair mode)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -a, --all
          Show all workspace crates (ignore changes)

  -o, --output <OUTPUT>
          Write output to file (e.g., $GITHUB_OUTPUT)

      --explain
          Explain why each crate is affected

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail affected                     # Changes since origin/main
  cargo rail affected --since HEAD~5      # Changes in last 5 commits
  cargo rail affected --from abc --to def # Changes between two SHAs
  cargo rail affected --explain           # Show why each crate is affected
  cargo rail affected -f github-matrix    # Output for GitHub Actions matrix
  cargo rail affected -f names-only       # Just crate names, one per line
```

---

## cargo rail test

```
Run tests for affected crates only

Usage: cargo rail test [OPTIONS] [-- <TEST_ARGS>...]

Arguments:
  [TEST_ARGS]...
          Pass additional arguments to the test runner

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --since <SINCE>
          Git ref to compare against (auto-detects origin/main or origin/master)

  -a, --all
          Skip change detection and run all tests

      --json
          Output in JSON format (shorthand for -f json)

      --skip-nextest
          Disable automatic use of cargo-nextest

      --explain
          Explain why tests are being run

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail test                         # Test affected crates
  cargo rail test --all                   # Test all crates
  cargo rail test -- --nocapture          # Pass args to test runner
  cargo rail test --explain               # Show why each crate is tested
```

---

## cargo rail unify

```
Unify workspace dependencies (replaces workspace-hack crates)

Usage: cargo rail unify [OPTIONS] [COMMAND]

Commands:
  undo  Restore manifests from a previous backup
  help  Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -c, --check
          Dry-run mode: preview changes without modifying files

      --json
          Output in JSON format (shorthand for -f json)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

      --exclude <EXCLUDE>
          Exclude dependencies from unification (comma-separated)

      --include <INCLUDE>
          Force include specific dependencies (comma-separated)

      --backup
          Create backups of all modified files

      --pin-transitives
          Pin transitive-only deps with fragmented features to workspace

      --include-renamed
          Include renamed dependencies (package = "...") in unification

      --skip-report
          Skip generating the unify report

      --report-path <REPORT_PATH>
          Custom path for the unify report (default: target/cargo-rail/unify-report.md)

      --show-diff
          Show diff of changes to each manifest

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail unify --check                # Preview changes (CI mode)
  cargo rail unify                        # Apply changes
  cargo rail unify --backup               # Apply with backup
  cargo rail unify --pin-transitives      # Pin fragmented deps (hakari replacement)
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups
```

---

## cargo rail init

```
Initialize configuration (rail.toml)

Usage: cargo rail init [OPTIONS]

Options:
  -o, --output <OUTPUT>  Output path for rail.toml [default: .config/rail.toml]
  -q, --quiet            Suppress progress messages (for CI/automation)
      --force            Overwrite existing configuration
      --json             Output in JSON format (shorthand for -f json)
      --non-interactive  Skip interactive prompts
  -c, --check            Dry-run mode: preview generated config without writing
  -h, --help             Print help
  -V, --version          Print version
```

---

## cargo rail split

```
Split a crate to a standalone repository with git history

Usage: cargo rail split [OPTIONS] <COMMAND>

Commands:
  init  Configure split for crate(s)
  run   Execute split operation
  help  Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail split init my-crate          # Configure split for my-crate
  cargo rail split init my-crate --check  # Preview generated config
  cargo rail split run my-crate --check   # Preview the split
  cargo rail split run my-crate           # Execute the split
  cargo rail split run --all              # Split all configured crates
```

---

## cargo rail sync

```
Sync changes between monorepo and split repos

Usage: cargo rail sync [OPTIONS] [CRATE_NAME]

Arguments:
  [CRATE_NAME]
          Crate name to sync (mutually exclusive with --all)

Options:
  -a, --all
          Sync all configured crates (mutually exclusive with crate name)

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --remote <REMOTE>
          Override remote repository

      --from-remote
          Sync from remote to monorepo only

      --to-remote
          Sync from monorepo to remote only

      --strategy <STRATEGY>
          Conflict resolution strategy

          Possible values:
          - ours:   Use the monorepo version (--ours)
          - theirs: Use the remote/split repo version (--theirs)
          - manual: Attempt automatic merge; create conflict markers if conflicts exist (default)
          - union:  Combine both versions line-by-line (union merge)
          
          [default: manual]

      --no-protected-branches
          Allow direct commits to protected branches

  -c, --check
          Dry-run mode: preview changes without executing

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail sync my-crate                # Bidirectional sync
  cargo rail sync my-crate --to-remote    # Push monorepo -> split repo
  cargo rail sync my-crate --from-remote  # Pull split repo -> monorepo (PR branch)
  cargo rail sync --all                   # Sync all configured crates
```

---

## cargo rail release

```
Publish releases (version bump, changelog, tag, publish)

Usage: cargo rail release [OPTIONS] <COMMAND>

Commands:
  init  Configure release settings
  run   Execute release (plan or publish)
  help  Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail release init my-crate              # Configure release for my-crate
  cargo rail release run my-crate --check       # Preview release plan
  cargo rail release run my-crate               # Release (patch bump)
  cargo rail release run my-crate --bump minor
  cargo rail release run --all --bump patch     # Release all crates
  cargo rail release run my-crate --skip-publish  # Tag only, no crates.io
```

---

## cargo rail check

```
Validate release readiness

Usage: cargo rail check [OPTIONS] [CRATE_NAMES]...

Arguments:
  [CRATE_NAMES]...
          Crate name(s) to check (mutually exclusive with --all)

Options:
  -a, --all
          Check all workspace crates (mutually exclusive with crate names)

  -q, --quiet
          Suppress progress messages (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

      --json
          Output in JSON format (shorthand for -f json)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

## cargo rail clean

```
Clean generated artifacts (cache, backups, reports)

Usage: cargo rail clean [OPTIONS]

Options:
      --cache    Clean metadata cache only
  -q, --quiet    Suppress progress messages (for CI/automation)
      --backups  Prune old backups
      --json     Output in JSON format (shorthand for -f json)
      --reports  Clean generated reports
  -c, --check    Dry-run mode: preview what would be cleaned
  -h, --help     Print help
  -V, --version  Print version
```

---

## cargo rail config

```
Configuration management

Usage: cargo rail config [OPTIONS] <COMMAND>

Commands:
  validate  Validate the configuration file
  help      Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet    Suppress progress messages (for CI/automation)
      --json     Output in JSON format (shorthand for -f json)
  -h, --help     Print help
  -V, --version  Print version
```
