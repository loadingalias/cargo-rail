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
  affected     Show which crates are affected by changes
  test         Run tests for affected crates only
  unify        Unify workspace dependencies (replaces workspace-hack crates)
  init         Initialize configuration (rail.toml)
  split        (Advanced) Split a crate to a standalone repository with git history
  sync         (Advanced) Sync changes between monorepo and split repos
  release      Publish releases (version bump, changelog, tag, publish)
  clean        Clean generated artifacts (cache, backups, reports)
  config       Configuration management
  completions  Generate shell completions
  help         Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output in JSON format (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
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
          Git ref to compare against (auto-detects default branch)

      --from <FROM>
          Start ref (for SHA pair mode)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --to <TO>
          End ref (for SHA pair mode)

      --merge-base
          Use merge-base with default branch (better for feature branches)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -a, --all
          Show all workspace crates (ignore changes)

  -o, --output <PATH>
          Write output to file (appends to existing content)

      --explain
          Explain why each crate is affected

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail affected                     # Changes since default branch
  cargo rail affected --merge-base        # Changes since branch point (CI recommended)
  cargo rail affected --since HEAD~5      # Changes in last 5 commits
  cargo rail affected --from abc --to def # Changes between two SHAs
  cargo rail affected --explain           # Show why each crate is affected
  cargo rail affected -f github-matrix    # Output for GitHub Actions matrix
  cargo rail affected -f names-only       # Just crate names, one per line

CI tip: Use --merge-base for PRs to detect only your branch's changes,
even if the target branch has moved forward.
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
          Git ref to compare against (auto-detects default branch)

      --json
          Output in JSON format (shorthand for -f json)

      --merge-base
          Use merge-base with default branch (better for feature branches)

  -a, --all
          Skip change detection and run all tests

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --skip-nextest
          Disable automatic use of cargo-nextest

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --explain
          Explain why tests are being run

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail test                         # Test affected crates
  cargo rail test --merge-base            # Test changes since branch point (CI)
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

      --config <PATH>
          Path to rail.toml config file (bypass search order)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

      --backup
          Create backups of all modified files

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --skip-report
          Skip generating the unify report

      --report-path <REPORT_PATH>
          Custom path for the unify report (default: target/cargo-rail/unify-report.md)

      --show-diff
          Show diff of changes to each manifest

      --explain
          Explain why each decision was made

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail unify --check                # Preview changes (CI mode)
  cargo rail unify --check --explain      # Show why each decision was made
  cargo rail unify                        # Apply changes
  cargo rail unify --backup               # Apply with backup
  cargo rail unify --show-diff            # Show manifest changes
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups
```

---

### cargo rail unify undo

```
Restore manifests from a previous backup

Usage: cargo rail unify undo [OPTIONS]

Options:
      --list                   List available backups instead of restoring
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --backup-id <BACKUP_ID>  Specific backup ID to restore (defaults to most recent)
      --json                   Output in JSON format (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

## cargo rail init

```
Initialize configuration (rail.toml)

Usage: cargo rail init [OPTIONS]

Options:
  -o, --output <OUTPUT>
          Output path for rail.toml
          
          [default: .config/rail.toml]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --force
          Overwrite existing configuration

      --json
          Output in JSON format (shorthand for -f json)

  -c, --check
          Dry-run mode: preview generated config without writing

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail init                       # Generate .config/rail.toml
  cargo rail init --check               # Preview generated config
  cargo rail init -o rail.toml          # Custom output path
  cargo rail init --force               # Overwrite existing config
```

---

## cargo rail split

```
(Advanced) Split a crate to a standalone repository with git history

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

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

This is an advanced feature for extracting crates to standalone repositories
while preserving git history. Most teams should start with 'affected', 'test',
and 'unify' before using split/sync.

Examples:
  cargo rail split init my-crate          # Configure split for my-crate
  cargo rail split init my-crate --check  # Preview generated config
  cargo rail split run my-crate --check   # Preview the split
  cargo rail split run my-crate           # Execute the split
  cargo rail split run --all              # Split all configured crates
```

---

### cargo rail split init

```
Configure split for crate(s)

Usage: cargo rail split init [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...  Crate name(s) to configure

Options:
  -c, --check                  Preview generated config without writing
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output in JSON format (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

### cargo rail split run

```
Execute split operation

Usage: cargo rail split run [OPTIONS] [CRATE]

Arguments:
  [CRATE]
          Crate name to split (mutually exclusive with --all)

Options:
  -a, --all
          Split all configured crates

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --remote <REMOTE>
          Override remote repository

  -c, --check
          Dry-run mode: preview changes

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --allow-dirty
          Allow running on dirty worktree (uncommitted changes)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -y, --yes
          Skip confirmation prompts (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

## cargo rail sync

```
(Advanced) Sync changes between monorepo and split repos

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

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --from-remote
          Sync from remote to monorepo only

      --to-remote
          Sync from monorepo to remote only

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --strategy <STRATEGY>
          Conflict resolution strategy

          Possible values:
          - ours:   Use the monorepo version (--ours)
          - theirs: Use the remote/split repo version (--theirs)
          - manual: Attempt automatic merge; create conflict markers if conflicts exist (default)
          - union:  Combine both versions line-by-line (union merge)
          
          [default: manual]

  -c, --check
          Dry-run mode: preview changes without executing

      --allow-dirty
          Allow running on dirty worktree (uncommitted changes)

  -y, --yes
          Skip confirmation prompts (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

This is an advanced feature for bidirectional sync between monorepo and split
repositories. Requires 'split' to be configured first.

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
  init   Configure release settings
  run    Execute release (plan or publish)
  check  Validate release readiness
  help   Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail release init my-crate              # Configure release for my-crate
  cargo rail release check my-crate             # Validate release readiness
  cargo rail release check my-crate --extended  # Run extended checks (dry-run, MSRV)
  cargo rail release run my-crate --check       # Preview release plan
  cargo rail release run my-crate               # Release (patch bump)
  cargo rail release run my-crate --bump minor
  cargo rail release run my-crate --bump prerelease  # 1.0.0 -> 1.0.0-rc.1
  cargo rail release run my-crate --bump release     # 1.0.0-rc.2 -> 1.0.0
  cargo rail release run --all --bump patch     # Release all crates
  cargo rail release run my-crate --skip-publish  # Tag only, no crates.io
```

---

### cargo rail release init

```
Configure release settings

Usage: cargo rail release init [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...  Crate name(s) to configure (optional)

Options:
  -c, --check                  Preview generated config without writing
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output in JSON format (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

### cargo rail release run

```
Execute release (plan or publish)

Usage: cargo rail release run [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to release (mutually exclusive with --all)

Options:
  -a, --all
          Release all workspace crates

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --bump <BUMP>
          Version bump [major, minor, patch, prerelease, release, or "x.y.z"]
          
          [default: patch]

      --json
          Output in JSON format (shorthand for -f json)

  -c, --check
          Dry-run mode: preview release plan

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --skip-publish
          Skip publishing to crates.io

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --skip-tag
          Skip git tag creation

  -y, --yes
          Skip confirmation prompts and allow non-default branch

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail release check

```
Validate release readiness

Usage: cargo rail release check [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to check (mutually exclusive with --all)

Options:
  -a, --all
          Check all workspace crates (mutually exclusive with crate names)

  -q, --quiet
          Suppress progress messages (for CI/automation)

  -e, --extended
          Run extended validation (cargo publish --dry-run, MSRV check)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

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
      --cache
          Clean metadata cache only

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --backups
          Prune old backups

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --reports
          Clean generated reports

  -c, --check
          Dry-run mode: preview what would be cleaned

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail clean                      # Clean all artifacts
  cargo rail clean --cache              # Clean metadata cache only
  cargo rail clean --backups            # Prune old backups
  cargo rail clean --reports            # Clean generated reports
  cargo rail clean --check              # Preview what would be cleaned
```

---

## cargo rail config

```
Configuration management

Usage: cargo rail config [OPTIONS] <COMMAND>

Commands:
  locate    Print the path to the active config file
  print     Print the effective configuration with defaults
  validate  Validate the configuration file
  sync      Sync configuration: add missing fields and update targets
  help      Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail config locate              # Show which config file is active
  cargo rail config print               # Show effective config with defaults
  cargo rail config validate            # Validate rail.toml
  cargo rail config validate -f json    # JSON output for CI
  cargo rail config sync --check        # Preview config updates
  cargo rail config sync                # Add missing fields, sync targets
```

---

### cargo rail config locate

```
Print the path to the active config file

Shows which config file is being used. Searches in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml

Usage: cargo rail config locate [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail config print

```
Print the effective configuration with defaults

Shows the merged configuration: user settings plus defaults for any unset fields. Useful for debugging and understanding what cargo-rail will actually use.

Usage: cargo rail config print [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail config validate

```
Validate the configuration file

Checks for parse errors, unknown keys, and semantic issues. By default, unknown keys warn locally but error in CI environments (detected via CI, GITHUB_ACTIONS, GITLAB_CI, or CIRCLECI env vars).

Usage: cargo rail config validate [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --strict
          Treat warnings as errors (auto-enabled in CI)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --no-strict
          Never treat warnings as errors (overrides CI auto-detection)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail config sync

```
Sync configuration: add missing fields and update targets

Scans the workspace for target triples and adds any missing config fields with their default values. Preserves all existing settings, comments, and formatting.

Use this after upgrading cargo-rail to get new configuration options.

Usage: cargo rail config sync [OPTIONS]

Options:
  -c, --check
          Preview changes without modifying rail.toml

  -q, --quiet
          Suppress progress messages (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:          Human-readable text output (default)
          - json:          Machine-readable JSON output
          - names-only:    Names only, one per line
          - cargo-args:    Cargo -p flag format: -p crate1 -p crate2
          - github:        GitHub Actions output format for $GITHUB_OUTPUT
          - github-matrix: GitHub Actions matrix format for strategy.matrix
          - jsonl:         JSON Lines format (one object per line)
          
          [default: text]

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

## cargo rail completions

```
Generate shell completions

Usage: cargo rail completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>
          Shell to generate completions for
          
          [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output in JSON format (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail completions bash           # Output bash completions
  cargo rail completions zsh            # Output zsh completions
  cargo rail completions fish           # Output fish completions
  cargo rail completions powershell     # Output PowerShell completions

Installation:
  # Bash (~/.bashrc)
  eval "$(cargo rail completions bash)"

  # Zsh (~/.zshrc)
  eval "$(cargo rail completions zsh)"

  # Fish (~/.config/fish/config.fish)
  cargo rail completions fish | source

  # PowerShell
  cargo rail completions powershell | Out-String | Invoke-Expression
```
