# Command Reference

> Auto-generated from `cargo rail --help`. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`

This is the exhaustive CLI surface. Start with `cargo rail plan --merge-base --explain` to inspect affected work,
then use `cargo rail run --merge-base --dry-run --print-cmd` to preview execution. Adopt dependency, release, and
split/sync workflows independently; they share one captured workspace view rather than rebuilding Cargo state in
separate tools.

---

## cargo rail

```
The Rust workspace engine.

Cargo-Rail turns Cargo's resolved workspace model and an exact source snapshot into affected CI, verified compiler reuse,
dependency coherence, exact-SHA releases, and crate synchronization.

Quick start:
  cargo rail plan --merge-base --explain          # Inspect affected work and reasoning
  cargo rail run --merge-base --dry-run --print-cmd  # Preview selected actions
  cargo rail run --merge-base --profile ci        # Run affected CI actions
  cargo rail unify --check --explain              # Inspect dependency changes (exit 1 when pending)

Docs: https://github.com/loadingalias/cargo-rail

Usage: cargo rail [OPTIONS] <COMMAND>

Commands:
  run          Execute planner-selected actions
  doctor       Inspect action hermeticity and native-cache capability
  cache        Inspect or reclaim explicitly scoped cache state
  plan         Build a deterministic file-first change plan
  unify        Unify workspace dependencies (replaces workspace-hack crates)
  init         Initialize configuration (rail.toml)
  split        (Advanced) Split a crate to a standalone repository with git history
  sync         (Advanced) Sync changes between monorepo and split repos
  release      Publish releases (version bump, changelog, tag, publish)
  change       Manage pending release intent files
  clean        Clean generated artifacts (cache, backups, reports)
  config       Configuration management
  hash         Compute a portable planner identity (not a cache key)
  diff-hash    Explain why two portable planner identities differ
  graph        Planner reasoning graph for explainability
  completions  Generate shell completions
  help         Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

## cargo rail run

```
Execute planner-selected actions

Usage: cargo rail run [OPTIONS] [-- <RUN_ARGS>...]

Arguments:
  [RUN_ARGS]...
          Pass harness args after `--` for tests; runner args for other actions

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --since <SINCE>
          Git ref to compare against (auto-detects default branch)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --merge-base
          Use merge-base with default branch (better for feature branches)

  -a, --all
          Skip change detection and run all workspace crates

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --action <ACTION>
          Action(s) to execute (repeatable; --surface is a compatibility alias)

          [alias: --surface]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --profile <PROFILE>
          Named profile to map to one or more actions

      --workflow <WORKFLOW>
          Named workflow mapped to a profile via `[run.workflow]`

      --dry-run
          Preview selected execution without spawning subprocesses

      --hermetic
          Execute supported Rust actions in fresh isolated roots

      --no-cache
          Disable all Cargo-Rail build-result cache reads and writes for this execution

  -f, --format <FORMAT>
          Dry-run action plan format (json/github require --dry-run)

          Possible values:
          - text:   Human-readable execution or preview output (default)
          - json:   Versioned machine-readable action plan
          - github: GitHub Actions key/value output containing the ordered action IDs

          [default: text]

      --generated <GENERATED>
          Generated-output behavior

          Possible values:
          - check:      Run each generator's read-only staleness check
          - regenerate: Update each generator's declared outputs

          [default: regenerate]

      --print-cmd
          Print command(s) prior to execution

      --explain
          Explain why actions and targets were selected

      --ignore-bin-crates
          Ignore binary-only crates (packages with `[[bin]]` but no lib target)

      --skip-nextest
          Disable automatic use of cargo-nextest

      --test-runner <TEST_RUNNER>
          Test runner backend (auto selects nextest when available)

          Possible values:
          - auto:    Prefer nextest when installed and otherwise use Cargo
          - cargo:   Require `cargo test`
          - nextest: Require `cargo nextest run`

          [default: auto]

      --cargo-test-arg <ARG>
          Pass an option only to `cargo test` (repeatable)

      --nextest-arg <ARG>
          Pass an option only to `cargo nextest run` (repeatable)

      --test-filter <FILTER>
          Portable test-name filter placed before the test-binary separator

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail run                              # Execute planner-selected test action
  cargo rail run --merge-base                 # Compare from branch point (CI)
  cargo rail run --action build --action test
  cargo rail run --profile ci                 # Built-in profile (local|ci|nightly)
  cargo rail run --workflow commit            # Resolve profile from [run.workflow.commit]
  cargo rail run --profile bench              # User-defined profile from [run.profile.bench]
  cargo rail run --all --action test          # Force full test run
  cargo rail run --all --action build --hermetic  # Prove a locked/offline Cargo check
  cargo rail run --dry-run --print-cmd        # Preview exact execution
  cargo rail run --dry-run -f json            # Versioned CI action plan
  cargo rail run --dry-run -f github          # GitHub Actions key=value plan
  cargo rail run --action codegen --generated check
  cargo rail run --test-filter parser         # Portable test-name filter
  cargo rail run --cargo-test-arg=--all-features --test-runner cargo
  cargo rail run --nextest-arg=-P --nextest-arg=commit
  cargo rail run -- --nocapture               # Pass harness args after --
```

---

## cargo rail doctor

```
Inspect action hermeticity and native-cache capability

Usage: cargo rail doctor [OPTIONS] <COMMAND>

Commands:
  hermeticity   Explain action-key eligibility and every incomplete input boundary
  native-cache  Inspect the exact native-cache compiler identity
  help          Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

### cargo rail doctor hermeticity

```
Explain action-key eligibility and every incomplete input boundary

Usage: cargo rail doctor hermeticity [OPTIONS]

Options:
      --action <ACTION>
          Action(s) to inspect (repeatable)

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --profile <PROFILE>
          Named profile to inspect

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workflow <WORKFLOW>
          Named workflow mapped to a profile via `[run.workflow]`

      --generated <GENERATED>
          Generated-output behavior to inspect

          Possible values:
          - check:      Run each generator's read-only staleness check
          - regenerate: Update each generator's declared outputs

          [default: regenerate]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

      --ignore-bin-crates
          Ignore binary-only crates

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail doctor native-cache

```
Inspect the exact native-cache compiler identity

Usage: cargo rail doctor native-cache [OPTIONS]

Options:
  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

## cargo rail cache

```
Inspect or reclaim explicitly scoped cache state

Usage: cargo rail cache [OPTIONS] <COMMAND>

Commands:
  status  Report exact bytes, counts, bounds, leases, and ownership scope
  clean   Reclaim one explicitly selected cache scope
  help    Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail cache status                         # Inspect workspace and shared local cache state
  cargo rail cache status --scope local -f json  # Inspect the shared local CAS only
  cargo rail cache clean --scope workspace --check  # Preview workspace cache reclamation
  cargo rail cache clean --scope local            # Remove the validated cross-workspace CAS
```

---

### cargo rail cache status

```
Report exact bytes, counts, bounds, leases, and ownership scope

Usage: cargo rail cache status [OPTIONS]

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --scope <SCOPE>
          Cache scope to inspect

          Possible values:
          - workspace: Reconstructible cache state inside the selected workspace
          - local:     The validated user-wide CAS shared by local workspaces
          - all:       Both workspace state and the shared local CAS

          [default: all]

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail cache clean

```
Reclaim one explicitly selected cache scope

Usage: cargo rail cache clean [OPTIONS] --scope <SCOPE>

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --scope <SCOPE>
          Cache scope to reclaim; required to prevent accidental cross-workspace deletion

          Possible values:
          - workspace: Reconstructible cache state inside the selected workspace
          - local:     The validated user-wide CAS shared by local workspaces
          - all:       Both workspace state and the shared local CAS

  -c, --check
          Preview exact bytes and paths without deleting them

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          - text:         Human-readable text output (default)
          - json:         Machine-readable JSON output
          - github:       GitHub Actions output format for $GITHUB_OUTPUT
          - github-debug: GitHub Actions output with embedded planner contract for debugging

          [default: text]

  -o, --output <PATH>
          Write output to file (overwrites existing content)

      --explain
          Show concise human reasoning chain

      --confidence-profile <PROFILE>
          Planner confidence profile override (strict|balanced|fast)

          [possible values: strict, balanced, fast]

      --schema
          Print the versioned planner JSON Schema and exit

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
  cargo rail plan --schema                  # Print the versioned JSON Schema
  cargo rail plan -f json                   # Full machine-readable contract
  cargo rail plan -f github                 # Compact GitHub Actions key=value output
  cargo rail plan -f github-debug           # GitHub Actions output plus plan_json
```

---

## cargo rail unify

```
Unify workspace dependencies (replaces workspace-hack crates)

Usage: cargo rail unify [OPTIONS] [COMMAND]

Commands:
  doctor  Inspect Cargo resolution semantics without changing files
  undo    Restore manifests from a previous backup
  help    Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

  -c, --check
          Check for pending changes without modifying files (exit 1 when pending)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

      --backup
          Create backups of all modified files

      --skip-report
          Skip generating the unify report

      --report-path <REPORT_PATH>
          Custom path for the unify report (default: target/cargo-rail/unify-report.md)

  -o, --output <PATH>
          Write output to file (overwrites existing content)

      --show-diff
          Show diff of changes to each manifest

      --explain
          Explain why each decision was made

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail unify --check                # Check for pending changes (exit 1)
  cargo rail unify --check --explain      # Show why each decision was made
  cargo rail unify --check -f json -o out.json  # Write JSON output to file
  cargo rail unify                        # Apply changes
  cargo rail unify --backup               # Apply with backup
  cargo rail unify --show-diff            # Show manifest changes
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups
```

---

### cargo rail unify doctor

```
Inspect Cargo resolution semantics without changing files

Usage: cargo rail unify doctor [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail unify undo

```
Restore manifests from a previous backup

Usage: cargo rail unify undo [OPTIONS]

Options:
      --list                   List available backups instead of restoring
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --backup-id <BACKUP_ID>  Specific backup ID to restore (defaults to most recent)
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --dry-run
          Preview generated config without writing

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail init                       # Generate .config/rail.toml
  cargo rail init --dry-run             # Preview generated config
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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
  cargo rail split init my-crate --dry-run  # Preview generated config
  cargo rail split run my-crate --check   # Check for a pending split (exit 1)
  cargo rail split run my-crate           # Execute the split
  cargo rail split run my-crate --yes     # Non-interactive apply confirmation
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
      --dry-run                Preview generated config without writing
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --remote <REMOTE>
          Override remote repository

  -c, --check
          Check for pending split changes (exit 1 when pending)

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
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON output
          - names-only: Names only, one per line
          - jsonl:      JSON Lines format (one object per line)

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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          Check for pending changes without executing (exit 1 when pending)

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --resume <RECEIPT>
          Resume a manually resolved sync conflict receipt

      --allow-dirty
          Allow running on dirty worktree (uncommitted changes)

  -y, --yes
          Skip confirmation prompts (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

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
  cargo rail sync my-crate --to-remote --yes  # Non-interactive apply confirmation
  cargo rail sync --all                   # Sync all configured crates
```

---

## cargo rail release

```
Publish releases (version bump, changelog, tag, publish)

Usage: cargo rail release [OPTIONS] <COMMAND>

Commands:
  init      Configure release settings
  run       Execute release (plan or publish)
  check     Validate release readiness
  finalize  Finalize a merged release PR (tag, push, publish)
  resume    Resume an interrupted release from its durable state file
  status    Show durable release state and the safe recovery command
  abort     Abort an active release that has not reached remote side effects
  help      Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
  cargo rail release init my-crate --dry-run    # Preview generated config
  cargo rail release check my-crate             # Validate release readiness
  cargo rail release check my-crate --extended  # Run extended checks (dry-run, MSRV)
  cargo rail release run my-crate --check       # Check for a pending release (exit 1)
  cargo rail release run my-crate               # Release from reviewed change intent
  cargo rail release run my-crate --include-dependents  # Release selected crate plus dependent closure
  cargo rail release run my-crate --yes         # Non-interactive apply confirmation
  cargo rail release run my-crate --bump auto   # Infer each bump from the configured release source
  cargo rail release run --all --bump auto --pr # Open a release PR with bumps/changelogs only
  cargo rail release finalize --all             # Tag/publish after the release PR merges
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
      --dry-run                Preview generated config without writing
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
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
          Version bump [auto, major, minor, patch, prerelease, release, or "x.y.z"]

          [default: auto]

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

  -c, --check
          Check for a pending release plan (exit 1 when pending)

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

      --pr
          Prepare a release PR branch instead of tagging or publishing

      --include-dependents
          Expand explicit crate selection to include the full dependent closure

  -y, --yes
          Skip confirmation prompts and allow non-default branch

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --include-dependents
          Expand explicit crate selection to include the full dependent closure

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail release finalize

```
Finalize a merged release PR (tag, push, publish)

Usage: cargo rail release finalize [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to finalize (required unless --all)

Options:
  -a, --all
          Finalize all workspace crates with release notes for their current versions

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --skip-publish
          Skip publishing to crates.io

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --skip-tag
          Skip git tag creation

      --include-dependents
          Expand explicit crate selection to include the full dependent closure and version groups

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -y, --yes
          Skip confirmation prompts and allow non-default branch

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail release resume

```
Resume an interrupted release from its durable state file

Usage: cargo rail release resume [OPTIONS] <STATE>

Arguments:
  <STATE>  State path printed by the interrupted release

Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

### cargo rail release status

```
Show durable release state and the safe recovery command

Usage: cargo rail release status [OPTIONS] [STATE]

Arguments:
  [STATE]
          Inspect one state file instead of every known release transaction

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail release abort

```
Abort an active release that has not reached remote side effects

Usage: cargo rail release abort [OPTIONS] <STATE>

Arguments:
  <STATE>  State path printed by the active release

Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -y, --yes                    Confirm restoration of the pre-release local state
      --json                   Output as JSON where supported; rejected otherwise (shorthand for -f json)
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
  -h, --help                   Print help
  -V, --version                Print version
```

---

## cargo rail change

```
Manage pending release intent files

Usage: cargo rail change [OPTIONS] <COMMAND>

Commands:
  add     Create a pending change file
  status  Show pending change files
  check   Check that changed crates have pending change files
  help    Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail change add rail-core --bump minor --message "Added auto bump planning"
  cargo rail change add rail-core rail-cli --bump patch --message "Fixed release notes"
  cargo rail change add rail-core --bump patch --name fix-parser
  cargo rail change status
  cargo rail change status --format json
  cargo rail change check --merge-base --required
  cargo rail change check --since origin/main --format json

Omit --message in an interactive terminal to author in $VISUAL or $EDITOR.
Change files are consumed (deleted in the release commit) when released.
Consumption is all-or-nothing: a release plan that covers only some of a
file's crates is rejected so no pending intent is ever lost.
```

---

### cargo rail change add

```
Create a pending change file

Usage: cargo rail change add [OPTIONS] --bump <BUMP> [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) covered by this change

Options:
      --bump <BUMP>
          Release intent for the covered crate(s): none, patch, minor, major

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

  -m, --message <MESSAGE>
          User-facing changelog entry body (omit in a terminal to open $VISUAL/$EDITOR)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --name <SLUG>
          Override the generated filename slug

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON output
          - names-only: Names only, one per line

          [default: text]

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

---

### cargo rail change status

```
Show pending change files

Usage: cargo rail change status [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON output
          - names-only: Names only, one per line

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail change check

```
Check that changed crates have pending change files

Usage: cargo rail change check [OPTIONS]

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --since <REF>
          Compare against this git ref

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --merge-base
          Compare from the merge-base with the default branch

      --all
          Scan the full reachable history

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --required
          Require coverage for every changed crate, ignoring release.require_change_files

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON output
          - names-only: Names only, one per line

          [default: text]

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
          Clean validated local and workspace cache state

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --backups
          Prune old backups

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --reports
          Clean generated reports

  -c, --check
          Check for pending cleanup without deleting files (exit 1 when pending)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail clean                      # Clean all artifacts
  cargo rail clean --cache              # Clean validated local and workspace cache state
  cargo rail clean --backups            # Prune old backups
  cargo rail clean --reports            # Clean generated reports
  cargo rail clean --check              # Check for pending cleanup (exit 1)
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
  explain   Explain effective values, defaults, sources, and deprecations
  migrate   Apply explicit semantic configuration migrations
  help      Print this message or the help of the given subcommand(s)

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
  cargo rail config explain             # Explain effective values and sources
  cargo rail config migrate --check     # Check for pending semantic migrations
  cargo rail config migrate             # Apply explicit semantic migrations
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
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail config explain

```
Explain effective values, defaults, sources, and deprecations

Usage: cargo rail config explain [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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

### cargo rail config migrate

```
Apply explicit semantic configuration migrations

This never adds coded defaults. It only performs reviewed migrations for deprecated fields while preserving unrelated TOML formatting.

Usage: cargo rail config migrate [OPTIONS]

Options:
      --check
          Check for pending migrations without modifying rail.toml

  -q, --quiet
          Suppress progress messages (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
Compute a portable planner identity (not a cache key)

Usage: cargo rail hash [OPTIONS]

Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

      --since <SINCE>
          Git ref to compare against (auto-detects default branch)

      --from <FROM>
          Start ref (for SHA pair mode)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail hash                          # Portable identity of the current plan
  cargo rail hash --merge-base             # Identity for the merge-base comparison
  cargo rail hash -f json                  # Structured identity metadata
  cargo rail diff-hash plan-a.json plan-b.json
  cargo rail diff-hash plan-a.json plan-b.json -f json
```

---

## cargo rail diff-hash

```
Explain why two portable planner identities differ

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
          - text: Human-readable text output (default)
          - json: Machine-readable JSON output

          [default: text]

  -q, --quiet
          Suppress progress messages (for CI/automation)

      --json
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Examples:
  cargo rail hash                          # Portable identity of the current plan
  cargo rail hash --merge-base             # Identity for the merge-base comparison
  cargo rail hash -f json                  # Structured identity metadata
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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
          Write output to file (overwrites existing content)

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
          Output as JSON where supported; rejected otherwise (shorthand for -f json)

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
