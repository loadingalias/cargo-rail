# Command Reference

> Auto-generated from `cargo rail --help`. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`

This is the exhaustive CLI surface. Start with `cargo rail plan --explain` to inspect required named work,
then pass each selected work item's typed Cargo arguments to Cargo, cargo-nextest, Just, or CI. Adopt dependency,
release, and split/sync workflows independently; they share one captured workspace view rather than rebuilding Cargo
state in separate tools.

---

## cargo rail

```
Cargo-Rail turns one captured Rust workspace into trustworthy plans, checks, mutations, releases, and compiler reuse.

Common inspection:
  cargo rail plan                         # Decide required repository work
  cargo rail surface --check              # Check Rust visibility findings
  cargo rail config explain               # Show configured policy overrides
  cargo rail cache status                 # Inspect compiler-cache health

Workspace mutation:
  cargo rail init                         # Create sparse repository policy
  cargo rail unify apply                  # Apply dependency coherence edits
  cargo rail change add --help            # Record release intent
  cargo rail clean --help                 # Select owned artifacts to remove

Advanced and external operations:
  cargo rail split --help                 # Extract a crate with Git history
  cargo rail sync --help                  # Synchronize split repositories
  cargo rail release --help               # Prepare or publish exact-SHA releases
  cargo rail doctor --help                # Inspect compiler integration

Docs: https://github.com/loadingalias/cargo-rail

Usage: cargo-rail [OPTIONS] <COMMAND>

Commands:
  doctor       Inspect native compiler-cache capability
  cache        Inspect or reclaim explicitly scoped cache state
  plan         Build an evidence-backed named-work plan
  surface      Analyze and repair complete Rust declaration reachability and visibility
  unify        Analyze and repair workspace dependency coherence
  init         Initialize configuration (rail.toml)
  split        (Advanced) Split a crate to a standalone repository with git history
  sync         (Advanced) Sync changes between monorepo and split repos
  release      Publish releases (version bump, changelog, tag, publish)
  change       Manage pending release intent files
  clean        Clean generated artifacts owned by the current workspace
  config       Configuration management
  completions  Generate shell completions
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail doctor

```
Inspect native compiler-cache capability

Usage: cargo-rail doctor [OPTIONS] <COMMAND>

Commands:
  native-cache  Inspect the exact native-cache compiler identity
  help          Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version

Global Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -v, --verbose                Show bounded operational detail
      --json                   Output as JSON where supported; rejected otherwise
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
```

---

### cargo rail doctor native-cache

```
Inspect the exact native-cache compiler identity

Usage: cargo-rail doctor native-cache [OPTIONS]

Options:
  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail cache

```
Inspect or reclaim explicitly scoped cache state

Usage: cargo-rail cache [OPTIONS] <COMMAND>

Commands:
  setup      Install or repair transparent verified compiler reuse
  normalize  (Advanced) Validate and normalize one machine-owned remote cache URL without network access
  status     Report cache installation and owned-storage health
  recover    Quarantine a selected markerless CAS and create a fresh owned authority
  clean      Reclaim one explicitly selected cache scope
  remove     Remove only transparent compiler-cache state owned by the setup receipt
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Remote URLs, credentials, provider environments, and distributed execution are machine-owned authority. Use setup
flags only after qualification has established the required trust domain, root portability, and worker identity.

Examples:
  cargo rail cache setup --check                  # Preview transparent compiler reuse setup
  cargo rail cache setup                          # Install or repair the Cargo wrapper
  cargo rail cache setup --remote URL --root-portability remap  # Qualify cross-root L2 reuse
  cargo rail cache status                         # Inspect workspace and shared local cache state
  cargo rail cache status --scope local --json   # Inspect the shared local CAS only
  cargo rail cache recover --check                # Preview byte-preserving markerless CAS recovery
  cargo rail cache recover                        # Quarantine the old tree and create a fresh CAS
  cargo rail cache clean --scope workspace --check  # Preview workspace cache reclamation
  cargo rail cache clean --scope local            # Remove the validated cross-workspace CAS
  cargo rail cache remove --check                 # Preview exact setup-state removal
  cargo rail cache remove                         # Remove setup state but preserve the CAS
```

---

### cargo rail cache setup

```
Install or repair transparent verified compiler reuse

Usage: cargo-rail cache setup [OPTIONS]

Options:
      --local-dir <PATH>
          Local cache base directory (defaults to Cargo home)

      --max-size <SIZE>
          Positive binary byte size such as 10GiB

      --remote <URL>
          Machine-owned remote cache URL to persist with this installation

      --remote-mode <MODE>
          Maximum remote authority; explicit selection defaults to read-write

          [possible values: read, read-write]

      --remote-environment <NAME>
          Additional reviewed compiler environment name admitted to L2 identity

      --root-portability <MODE>
          Cross-checkout authority: physical roots remain exact; remap qualifies portable L2 results

          [possible values: physical, remap]

      --local-only
          Remove persisted remote activation while preserving local reuse

      --distributed-local
          Enable the same-host distributed protocol qualification path

      --distributed-endpoint <IP:PORT>
          Mutually authenticated direct worker socket address

      --distributed-server-name <NAME>
          TLS DNS name required from the distributed worker certificate

      --distributed-capability <IDENTITY>
          Exact capability identity advertised by the selected worker

      --distributed-authority <PATH>
          PEM certificate authority for the distributed worker

      --distributed-client-certificate <PATH>
          PEM client certificate presented to the distributed worker

      --distributed-client-private-key <PATH>
          Private PEM key for the distributed client certificate

      --distributed-policy <MODE>
          Placement policy for an mTLS worker. Qualification samples every eligible miss; automatic placement requires retained evidence of a critical-path win

          [possible values: automatic, qualification]

  -c, --check
          Preview exact Cargo configuration and private-state changes

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail cache normalize

```
(Advanced) Validate and normalize one machine-owned remote cache URL without network access

Usage: cargo-rail cache normalize [OPTIONS] <URL>

Arguments:
  <URL>
          AWS S3, Azure Blob Storage, or Cloudflare R2 URL

Options:
      --mode <MODE>
          Maximum authority; explicit selection defaults to read-write

          [possible values: read, read-write]

      --environment <NAME>
          Additional reviewed compiler environment name admitted to L2 identity

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail cache status

```
Report cache installation and owned-storage health

Usage: cargo-rail cache status [OPTIONS]

Options:
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
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail cache recover

```
Quarantine a selected markerless CAS and create a fresh owned authority

Usage: cargo-rail cache recover [OPTIONS]

Options:
  -c, --check
          Preview the exact quarantine move without modifying cache state

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail cache clean

```
Reclaim one explicitly selected cache scope

Usage: cargo-rail cache clean [OPTIONS] --scope <SCOPE>

Options:
      --scope <SCOPE>
          Cache scope to reclaim; required to prevent accidental cross-workspace deletion

          Possible values:
          - workspace: Reconstructible cache state inside the selected workspace
          - local:     The validated user-wide CAS shared by local workspaces
          - all:       Both workspace state and the shared local CAS

  -c, --check
          Preview exact bytes and paths without deleting them

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail cache remove

```
Remove only transparent compiler-cache state owned by the setup receipt

Usage: cargo-rail cache remove [OPTIONS]

Options:
  -c, --check
          Preview exact Cargo configuration and private-state changes

  -f, --format <FORMAT>
          Report format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail plan

```
Build an evidence-backed named-work plan

Usage: cargo-rail plan [OPTIONS]

Options:
      --since <SINCE>
          Git ref to compare against (auto-detects default branch)

      --from <FROM>
          Start ref (for SHA pair mode)

      --to <TO>
          End ref (for SHA pair mode)

      --explain
          Show concise human reasoning chain

      --explain-work <WORK_ID>
          Explain one exact work decision, including when it was skipped

      --all
          Require every registered work item with full valid scope

      --evidence <PATH>
          Load portable compatible observed-input evidence

      --verify <PATH>
          Verify that the current checkout matches one saved plan

      --schema
          Print the versioned planner JSON Schema and exit

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail plan                           # Changes since the default-branch merge base
  cargo rail plan --json                    # Full machine-readable work contract
  cargo rail plan --since HEAD~5            # Changes in last 5 commits
  cargo rail plan --from abc --to def       # Changes between two SHAs
  cargo rail plan --explain                 # Explain required decisions
  cargo rail plan --explain-work cargo.test # Explain one decision, even when skipped
  cargo rail plan --all                     # Safely require every registered work item
  cargo rail plan --evidence inputs.json    # Use compatible observed-input evidence
  cargo rail plan --verify plan.json        # Revalidate a saved plan without executing it
  cargo rail plan --schema                  # Print the versioned JSON Schema
  cargo rail plan --json > plan.json        # Redirect the exact plan to a file
```

---

## cargo rail surface

```
Analyze and repair complete Rust declaration reachability and visibility

Usage: cargo-rail surface [OPTIONS]

Options:
      --prepare
          Prepare and authenticate the exact-toolchain Surface producer without analysis

      --check
          Fail on denied findings without modifying source (for CI)

      --fix
          Apply exact visibility reductions

      --resume <MANIFEST>
          Resume from a prior partial acquisition manifest

      --dry-run
          Render the exact mutation plan without writing

      --backup
          Create a bounded backup before applying visibility edits

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:   Human-readable text output (default)
          - json:   Machine-readable JSON compatibility spelling; prefer global `--json`
          - github: GitHub Actions key/value output

          [default: text]

  -o, --output <PATH>
          Write output to file (overwrites existing content)

      --explain
          Show the reason chain for every finding

      --only <LINT>
          Restrict reported findings to one or more exact lint classes

          [possible values: dead-public, unnecessary-public, unnecessary-restricted-visibility, unnecessary-crate-visibility]

      --schema
          Print the versioned surface JSON Schema and exit

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Set `[surface] enabled = true` to include this gate in planner-selected CI.
Set `[surface] consumer_scope = "workspace"` only when each closed compiler
crate has no consumers outside the captured workspace.

Examples:
  cargo rail surface                        # Inspect and report without modifying source
  cargo rail surface --prepare              # Prove exact-toolchain producer readiness
  cargo rail surface --check --explain      # Inspect complete Rust reachability
  cargo rail surface --check --json         # Emit the versioned machine contract
  cargo rail surface --resume MANIFEST --json  # Resume a partial compiler acquisition
  cargo rail surface --fix --dry-run --explain  # Preview exact visibility edits
  cargo rail surface --fix --backup         # Apply verified edits with recovery evidence
  cargo rail surface --schema               # Print the versioned JSON Schema
```

---

## cargo rail unify

```
Analyze and repair workspace dependency coherence

Usage: cargo-rail unify [OPTIONS] [COMMAND]

Commands:
  apply   Apply the exact dependency-coherence decision
  doctor  Inspect Cargo resolution semantics without changing files
  undo    Restore manifests from a previous backup
  help    Print this message or the help of the given subcommand(s)

Options:
  -c, --check
          Check for pending manifest changes without modifying manifests (exit 1 when pending)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

      --report
          Generate the dependency report

      --report-path <PATH>
          Durable report destination

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

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail unify                        # Preview dependency changes (exit 0)
  cargo rail unify --check                # Check for pending changes (exit 1)
  cargo rail unify --explain              # Show why each decision was made
  cargo rail unify --show-diff            # Show manifest changes
  cargo rail unify apply                  # Apply the current decision
  cargo rail unify apply --backup         # Apply with backup
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups
```

---

### cargo rail unify apply

```
Apply the exact dependency-coherence decision

Usage: cargo-rail unify apply [OPTIONS]

Options:
      --plan <PATH>
          Apply from a previously generated mutation plan file

      --backup
          Create backups of all modified files

      --report
          Generate the dependency report

      --report-path <PATH>
          Durable report destination

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail unify doctor

```
Inspect Cargo resolution semantics without changing files

Usage: cargo-rail unify doctor [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail unify undo

```
Restore manifests from a previous backup

Usage: cargo-rail unify undo [OPTIONS]

Options:
      --list
          List available backups instead of restoring

      --backup-id <BACKUP_ID>
          Specific backup ID to restore (defaults to most recent)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail init

```
Initialize configuration (rail.toml)

Usage: cargo-rail init [OPTIONS]

Options:
  -o, --output <OUTPUT>
          Output path for rail.toml

          [default: .config/rail.toml]

      --force
          Overwrite existing configuration

      --dry-run
          Preview generated config without writing

      --target <TRIPLE>
          Add an exact supported Cargo target triple (repeatable)

      --detect-targets
          Detect target triples from repository files

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail init                       # Generate .config/rail.toml
  cargo rail init --dry-run             # Preview generated config
  cargo rail init --target wasm32-wasip1 # Declare one supported target
  cargo rail init --detect-targets       # Opt in to repository target detection
  cargo rail init --force               # Overwrite existing config
```

---

## cargo rail split

```
(Advanced) Split a crate to a standalone repository with git history

Usage: cargo-rail split [OPTIONS] <COMMAND>

Commands:
  init  Configure split for crate(s)
  run   Execute split operation
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

This is an advanced feature for extracting crates to standalone repositories
while preserving git history. Most teams should start with 'plan', 'cache',
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

Usage: cargo-rail split init [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...  Crate name(s) to configure

Options:
      --dry-run  Preview generated config without writing
  -h, --help     Print help
  -V, --version  Print version

Global Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -v, --verbose                Show bounded operational detail
      --json                   Output as JSON where supported; rejected otherwise
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
```

---

### cargo rail split run

```
Execute split operation

Usage: cargo-rail split run [OPTIONS] [CRATE]

Arguments:
  [CRATE]
          Crate name to split (mutually exclusive with --all)

Options:
  -a, --all
          Split all configured crates

      --remote <REMOTE>
          Override remote repository

  -c, --check
          Check for pending split changes (exit 1 when pending)

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --allow-dirty
          Allow running on dirty worktree (uncommitted changes)

  -y, --yes
          Skip confirmation prompts (for CI/automation)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON compatibility spelling; prefer global `--json`
          - names-only: Names only, one per line
          - jsonl:      JSON Lines format (one object per line)

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail sync

```
(Advanced) Sync changes between monorepo and split repos

Usage: cargo-rail sync [OPTIONS] [CRATE_NAME]

Arguments:
  [CRATE_NAME]
          Crate name to sync (mutually exclusive with --all)

Options:
  -a, --all
          Sync all configured crates (mutually exclusive with crate name)

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
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

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

Usage: cargo-rail release [OPTIONS] <COMMAND>

Commands:
  init      Configure release settings
  run       Execute release (plan or publish)
  check     Validate the local release plan or publication readiness
  finalize  Finalize a merged release PR (tag, push, publish)
  resume    Resume an interrupted release from its durable state file
  status    Show durable release state and the safe recovery command
  abort     Abort an active release that has not reached remote side effects
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail release init my-crate              # Configure release for my-crate
  cargo rail release init my-crate --dry-run    # Preview generated config
  cargo rail release check my-crate                    # Validate the local release plan
  cargo rail release check my-crate --publication     # Validate registry publication readiness
  cargo rail release check my-crate --publication -e  # Run publish, MSRV, and semver checks
  cargo rail release run my-crate               # Prepare a local release without registry publication
  cargo rail release run my-crate --publish     # Match configured crates.io authority at invocation
  cargo rail release run my-crate --include-dependents  # Release selected crate plus dependent closure
  cargo rail release run my-crate --yes         # Non-interactive apply confirmation
  cargo rail release run my-crate --bump auto   # Infer each bump from the configured release source
  cargo rail release run --all --bump auto --pr # Open a release PR with bumps/changelogs only
  cargo rail release finalize --all --publish   # Match configured crates.io authority after PR merge
  cargo rail release run my-crate --bump minor
  cargo rail release run my-crate --bump prerelease  # 1.0.0 -> 1.0.0-rc.1
  cargo rail release run my-crate --bump release     # 1.0.0-rc.2 -> 1.0.0
  cargo rail release run --all --bump patch     # Release all crates
```

---

### cargo rail release init

```
Configure release settings

Usage: cargo-rail release init [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...  Crate name(s) to configure (optional)

Options:
      --dry-run  Preview generated config without writing
  -h, --help     Print help
  -V, --version  Print version

Global Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -v, --verbose                Show bounded operational detail
      --json                   Output as JSON where supported; rejected otherwise
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
```

---

### cargo rail release run

```
Execute release (plan or publish)

Usage: cargo-rail release run [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to release (mutually exclusive with --all)

Options:
  -a, --all
          Release all workspace crates

      --bump <BUMP>
          Version bump [auto, major, minor, patch, prerelease, release, or "x.y.z"]

          [default: auto]

  -c, --check
          Deprecated compatibility check spelling; use `release check`

      --plan <PATH>
          Apply from a previously generated mutation plan file

      --publish
          Positively authorize irreversible publication to crates.io

      --skip-tag
          Skip git tag creation

      --pr
          Prepare a release PR branch instead of tagging or publishing

      --include-dependents
          Expand explicit crate selection to include the full dependent closure

  -y, --yes
          Skip the interactive confirmation prompt

      --allow-non-default-branch
          Authorize release execution from a non-default branch

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail release check

```
Validate the local release plan or publication readiness

Usage: cargo-rail release check [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to check (mutually exclusive with --all)

Options:
  -a, --all
          Check all workspace crates (mutually exclusive with crate names)

      --bump <BUMP>
          Version bump [auto, major, minor, patch, prerelease, release, or "x.y.z"]

          [default: auto]

      --publication
          Validate registry publication readiness instead of the local release plan

  -e, --extended
          Run extended publication validation (publish dry-run, MSRV, optional semver checks)

      --skip-tag
          Exclude git tag creation from the local release plan

      --include-dependents
          Expand explicit crate selection to include the full dependent closure

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail release finalize

```
Finalize a merged release PR (tag, push, publish)

Usage: cargo-rail release finalize [OPTIONS] [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) to finalize (required unless --all)

Options:
  -a, --all
          Finalize all workspace crates with release notes for their current versions

      --publish
          Positively authorize irreversible publication to crates.io

      --skip-tag
          Skip git tag creation

      --include-dependents
          Expand explicit crate selection to include the full dependent closure and version groups

  -y, --yes
          Skip the interactive confirmation prompt

      --allow-non-default-branch
          Authorize release execution from a non-default branch

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail release resume

```
Resume an interrupted release from its durable state file

Usage: cargo-rail release resume [OPTIONS] <STATE>

Arguments:
  <STATE>  State path printed by the interrupted release

Options:
  -h, --help     Print help
  -V, --version  Print version

Global Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -v, --verbose                Show bounded operational detail
      --json                   Output as JSON where supported; rejected otherwise
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
```

---

### cargo rail release status

```
Show durable release state and the safe recovery command

Usage: cargo-rail release status [OPTIONS] [STATE]

Arguments:
  [STATE]
          Inspect one state file instead of every known release transaction

Options:
      --history
          Include terminal and reconstructed transaction history

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail release abort

```
Abort an active release that has not reached remote side effects

Usage: cargo-rail release abort [OPTIONS] <STATE>

Arguments:
  <STATE>  State path printed by the active release

Options:
  -y, --yes      Confirm restoration of the pre-release local state
  -h, --help     Print help
  -V, --version  Print version

Global Options:
  -q, --quiet                  Suppress progress messages (for CI/automation)
  -v, --verbose                Show bounded operational detail
      --json                   Output as JSON where supported; rejected otherwise
      --config <PATH>          Path to rail.toml config file (bypass search order)
      --workspace-root <PATH>  Workspace root directory (default: current directory)
```

---

## cargo rail change

```
Manage pending release intent files

Usage: cargo-rail change [OPTIONS] <COMMAND>

Commands:
  add     Create a pending change file
  status  Show pending change files
  check   Check that changed crates have pending change files
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

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

Usage: cargo-rail change add [OPTIONS] --bump <BUMP> [CRATE]...

Arguments:
  [CRATE]...
          Crate name(s) covered by this change

Options:
      --bump <BUMP>
          Release intent for the covered crate(s): none, patch, minor, major

  -m, --message <MESSAGE>
          User-facing changelog entry body (omit in a terminal to open $VISUAL/$EDITOR)

      --name <SLUG>
          Override the generated filename slug

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON compatibility spelling; prefer global `--json`
          - names-only: Names only, one per line

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail change status

```
Show pending change files

Usage: cargo-rail change status [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON compatibility spelling; prefer global `--json`
          - names-only: Names only, one per line

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail change check

```
Check that changed crates have pending change files

Usage: cargo-rail change check [OPTIONS]

Options:
      --since <REF>
          Compare against this git ref

      --merge-base
          Compare from the merge-base with the default branch

      --all
          Scan the full reachable history

      --required
          Require coverage for every changed crate, ignoring release.require_change_files

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text:       Human-readable text output (default)
          - json:       Machine-readable JSON compatibility spelling; prefer global `--json`
          - names-only: Names only, one per line

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail clean

```
Clean generated artifacts owned by the current workspace

Usage: cargo-rail clean [OPTIONS]

Options:
      --all
          Clean every eligible workspace-owned artifact class

      --cache
          Clean cache state owned by this workspace

      --prune-backups
          Prune backups beyond the configured retention bound

      --all-backups
          Delete every workspace backup

      --reports
          Clean generated reports

      --release-journal <ID_OR_PATH>
          Delete exactly one terminal release journal by transaction ID or state path

  -c, --check
          Check for pending cleanup without deleting files (exit 1 when pending)

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail clean --all                # Clean every eligible current-workspace artifact
  cargo rail clean --cache              # Clean current-workspace cache state
  cargo rail clean --prune-backups      # Prune backups beyond configured retention
  cargo rail clean --all-backups        # Delete every backup
  cargo rail clean --reports            # Clean generated reports
  cargo rail clean --release-journal ID # Delete one terminal release journal
  cargo rail clean --cache --check      # Check selected cleanup (exit 1 when pending)
```

---

## cargo rail config

```
Configuration management

Usage: cargo-rail config [OPTIONS] <COMMAND>

Commands:
  locate    Print the path to the active config file
  print     Print canonical effective configuration with defaults
  validate  Validate the configuration file
  explain   Explain effective values, defaults, sources, and deprecations
  migrate   Apply explicit semantic configuration migrations
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

Examples:
  cargo rail config locate              # Show which config file is active
  cargo rail config print               # Show effective config with defaults
  cargo rail config validate            # Validate rail.toml
  cargo rail config validate --json     # JSON output for CI
  cargo rail config explain             # Explain effective values and sources
  cargo rail config explain targets     # Explain one field in full
  cargo rail config explain --all       # Explain the complete field inventory
  cargo rail config migrate --check     # Check for pending semantic migrations
  cargo rail config migrate             # Apply explicit semantic migrations
```

---

### cargo rail config locate

```
Print the path to the active config file

Shows which config file is being used. Searches in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml

Usage: cargo-rail config locate [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail config print

```
Print canonical effective configuration with defaults

Shows the merged repository policy: user settings plus defaults for any unset fields. Text output is reusable `rail.toml` input and omits deprecated compatibility-only fields.

Usage: cargo-rail config print [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail config validate

```
Validate the configuration file

Checks for parse errors, unknown keys, and semantic issues. By default, unknown keys warn locally but error in CI environments (detected via CI, GITHUB_ACTIONS, GITLAB_CI, or CIRCLECI env vars).

Usage: cargo-rail config validate [OPTIONS]

Options:
  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

      --strict
          Treat warnings as errors (auto-enabled in CI)

      --no-strict
          Never treat warnings as errors (overrides CI auto-detection)

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail config explain

```
Explain effective values, defaults, sources, and deprecations

Usage: cargo-rail config explain [OPTIONS] [FIELD]...

Arguments:
  [FIELD]...
          Exact configuration field path(s) to explain in full

Options:
      --all
          Explain every known effective field

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

### cargo rail config migrate

```
Apply explicit semantic configuration migrations

This never adds coded defaults. It only performs reviewed migrations for deprecated fields while preserving unrelated TOML formatting.

Usage: cargo-rail config migrate [OPTIONS]

Options:
      --check
          Check for pending migrations without modifying rail.toml

  -f, --format <FORMAT>
          Output format

          Possible values:
          - text: Human-readable text output (default)
          - json: Machine-readable JSON compatibility spelling; prefer global `--json`

          [default: text]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)
```

---

## cargo rail completions

```
Generate shell completions

Usage: cargo-rail completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>
          Shell to generate completions for

          [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Global Options:
  -q, --quiet
          Suppress progress messages (for CI/automation)

  -v, --verbose
          Show bounded operational detail

      --json
          Output as JSON where supported; rejected otherwise

      --config <PATH>
          Path to rail.toml config file (bypass search order)

      --workspace-root <PATH>
          Workspace root directory (default: current directory)

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
