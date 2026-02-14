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
  run          Execute planner-selected surfaces
  plan         Build a deterministic file-first change plan
  unify        Unify workspace dependencies (replaces workspace-hack crates)
  init         Initialize configuration (rail.toml)
  split        (Advanced) Split a crate to a standalone repository with git history
  sync         (Advanced) Sync changes between monorepo and split repos
  release      Publish releases (version bump, changelog, tag, publish)
  clean        Clean generated artifacts (cache, backups, reports)
  config       Configuration management
  hash         Hash and compare planner contracts
  diff-hash    Explain why two planner hashes differ
  graph        Planner reasoning graph for explainability
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

## cargo rail run

```
Execute planner-selected surfaces

Usage: cargo rail run [OPTIONS] [-- <RUN_ARGS>...]

Arguments:
  [RUN_ARGS]...
          Pass additional arguments to the selected runner

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
          Skip change detection and run all workspace crates

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --surface <SURFACE>
          Surface(s) to execute (repeatable)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --profile <PROFILE>
          Named profile to map to one or more surfaces

      --workflow <WORKFLOW>
          Named workflow mapped to a profile via `[run.workflow]`

      --dry-run
          Preview selected execution without spawning subprocesses

      --print-cmd
          Print command(s) prior to execution

      --explain
          Explain why surfaces and targets were selected

      --ignore-bin-crates
          Ignore binary-only crates (packages with `[[bin]]` but no lib target)

      --skip-nextest
          Disable automatic use of cargo-nextest

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail run                              # Execute planner-selected test surface
  cargo rail run --merge-base                 # Compare from branch point (CI)
  cargo rail run --surface build --surface test
  cargo rail run --profile ci                 # Built-in profile (local|ci|nightly)
  cargo rail run --workflow commit            # Resolve profile from [run.workflow.commit]
  cargo rail run --profile bench              # User-defined profile from [run.profile.bench]
  cargo rail run --all --surface test         # Force full test run
  cargo rail run --dry-run --print-cmd        # Preview exact execution
  cargo rail run -- --nocapture               # Pass args to underlying runner
```

---

## cargo rail plan

```
Build a deterministic file-first change plan

Usage: cargo rail plan [OPTIONS]

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
          - text:   Human-readable text output (default)
          - json:   Machine-readable JSON output
          - github: GitHub Actions output format for $GITHUB_OUTPUT
          
          [default: text]

  -o, --output <PATH>
          Write output to file (appends to existing content)

      --explain
          Show concise human reasoning chain

      --confidence-profile <PROFILE>
          Planner confidence profile override (strict|balanced|fast)
          
          [possible values: strict, balanced, fast]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail plan                           # Changes since default branch
  cargo rail plan --merge-base              # Changes since branch point (CI recommended)
  cargo rail plan --confidence-profile strict  # Conservative planner profile
  cargo rail plan --since HEAD~5            # Changes in last 5 commits
  cargo rail plan --from abc --to def       # Changes between two SHAs
  cargo rail plan --explain                 # Show concise proof chain
  cargo rail plan -f json                   # Full machine-readable contract
  cargo rail plan -f github                 # GitHub Actions key=value output
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

      --plan <PATH>
          Apply from a previously generated mutation plan file

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output
          
          [default: text]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --backup
          Create backups of all modified files

      --skip-report
          Skip generating the unify report

      --report-path <REPORT_PATH>
          Custom path for the unify report (default: target/cargo-rail/unify-report.md)

  -o, --output <PATH>
          Write output to file (appends to existing content)

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
  cargo rail unify --check -f json -o out.json  # Write JSON output to file
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
while preserving git history. Most teams should start with 'plan', 'run',
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

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

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

      --plan <PATH>
          Apply from a previously generated mutation plan file

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

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --skip-publish
          Skip publishing to crates.io

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

## cargo rail hash

```
Hash and compare planner contracts

Usage: cargo rail hash [OPTIONS]

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

      --confidence-profile <PROFILE>
          Planner confidence profile override (strict|balanced|fast)
          
          [possible values: strict, balanced, fast]

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
  cargo rail hash                          # Hash current planner contract
  cargo rail hash --merge-base             # Hash planner contract at merge-base comparison
  cargo rail hash -f json                  # Structured hash output
  cargo rail diff-hash plan-a.json plan-b.json
  cargo rail diff-hash plan-a.json plan-b.json -f json
```

---

## cargo rail diff-hash

```
Explain why two planner hashes differ

Usage: cargo rail diff-hash [OPTIONS] <A> <B>

Arguments:
  <A>
          First planner JSON path

  <B>
          Second planner JSON path

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

Examples:
  cargo rail hash                          # Hash current planner contract
  cargo rail hash --merge-base             # Hash planner contract at merge-base comparison
  cargo rail hash -f json                  # Structured hash output
  cargo rail diff-hash plan-a.json plan-b.json
  cargo rail diff-hash plan-a.json plan-b.json -f json
```

---

## cargo rail graph

```
Planner reasoning graph for explainability

Usage: cargo rail graph [OPTIONS]

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

      --confidence-profile <PROFILE>
          Planner confidence profile override (strict|balanced|fast)
          
          [possible values: strict, balanced, fast]

      --dot
          Output GraphViz DOT instead of JSON

  -o, --output <PATH>
          Write output to file (appends to existing content)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail graph                             # Planner reasoning graph (json)
  cargo rail graph --merge-base                # Graph against merge-base comparison
  cargo rail graph --dot                       # GraphViz DOT output
  cargo rail graph --since HEAD~3 -o graph.dot # Write graph output to file
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
