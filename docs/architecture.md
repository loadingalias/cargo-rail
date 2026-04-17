# Architecture

## The Big Picture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              cargo rail <cmd>                               │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  main.rs                                                                    │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────────┐  │
│  │ Parse CLI   │ → │ Init output │ → │ Build ctx   │ → │ dispatch(cmd)   │  │
│  └─────────────┘   └─────────────┘   └─────────────┘   └─────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
           ┌──────────────────────────┼──────────────────────────┐
           ▼                          ▼                          ▼
    ┌─────────────┐            ┌─────────────┐            ┌─────────────┐
    │    plan     │            │   unify     │            │  release    │
    │ run(exec)   │            │   split     │            │  sync       │
    │             │            │             │            │             │
    └─────────────┘            └─────────────┘            └─────────────┘
           │                          │                          │
           └──────────────────────────┼──────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  WorkspaceContext (built once, passed everywhere)                           │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐    │
│  │   GitState    │ │  CargoState   │ │WorkspaceGraph │ │  RailConfig   │    │
│  │   (Arc)       │ │   (Arc)       │ │    (Arc)      │ │   (Arc)       │    │
│  └───────────────┘ └───────────────┘ └───────────────┘ └───────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
           │                          │                          │
           ▼                          ▼                          ▼
    ┌─────────────┐            ┌─────────────┐            ┌─────────────┐
    │  src/git/   │            │ src/cargo/  │            │ src/graph/  │
    │  system git │            │ metadata    │            │  petgraph   │
    └─────────────┘            │ manifests   │            └─────────────┘
                               │ unify       │
                               └─────────────┘
```

**The one rule that really matters:** WorkspaceContext loads once, passes by reference everywhere else. No command re-loads metadata.

---

## Module Map

| Module | What it does | Key files |
|--------|--------------|-----------|
| `commands/` | CLI handlers (one file = one command) | `cli.rs`, `mod.rs` (dispatch) |
| `workspace/` | WorkspaceContext + state management | `context.rs`, `view.rs`, `change_analyzer.rs` |
| `graph/` | Dependency graph (petgraph) | `core.rs`, `query.rs` |
| `cargo/` | Metadata loading, manifest ops, unify | `multi_target_metadata.rs`, `unify_analyzer.rs` |
| `git/` | System git wrapper | `system.rs`, `ops.rs` |
| `change_detection/` | File classification | `classify.rs`, `presentation.rs` |
| `split/` | Crate extraction with history | `engine.rs` |
| `sync/` | Bidirectional sync | `engine.rs`, `conflict.rs` |
| `release/` | Version, changelog, publish | `planner.rs`, `publisher.rs`, `validator.rs` |
| `config/` | rail.toml parsing | `mod.rs`, per-feature files |
| `backup/` | Undo support | |
| `toml/` | Lossless TOML editing | |
| `output.rs` | Quiet/JSON mode control | |
| `error.rs` | RailError + exit codes | |

---

## Core Type: WorkspaceContext

```rust
pub struct WorkspaceContext {
    pub workspace_root: PathBuf,
    pub git: Arc<GitState>,                         // Git ops
    pub cargo: Arc<CargoState>,                     // Cargo metadata (O(1) package lookup)
    pub graph: Arc<WorkspaceGraph>,                 // Dep graph
    pub config: Option<Arc<RailConfig>>,
}
```

**Why Arc?** Cheap cloning across threads. Heavy state loaded once, shared freely.

**Access Patterns:**

```rust
// Get changed files
let files = ctx.git.git().get_changed_files_between(from, to)?;

// Look up a package (O(1))
let pkg = ctx.cargo.get_package("my-crate").expect("workspace member exists");

// Find transitive dependents
let dependents = ctx.graph.transitive_dependents("my-crate")?;

// Get config (errors if not present)
let config = ctx.require_config()?;
```

---

## Data Flow

### Startup (main.rs)

```
Parse CLI (clap)
    ↓
Init output mode (quiet/JSON)
    ↓
Handle early commands (init, completions, config validate/sync/locate/print, unify undo)  ← These don't need full context
    ↓
Build WorkspaceContext  (~100-300ms)
    ├─ GitState         (~5ms)
    ├─ CargoState       (50-200ms, cached)
    ├─ WorkspaceGraph   (10-50ms)
    └─ RailConfig       (<5ms)
    ↓
dispatch(cmd, &ctx)
```

### Command Execution

```
dispatch(cmd, &ctx)
    ↓
Match command → call handler
    ↓
Handler uses ctx.{git,cargo,graph,config}
    ↓
Return RailResult<()>
```

Every command follows this pattern:

```rust
pub fn run_whatever(ctx: &WorkspaceContext, args: Args) -> RailResult<()> {
    let config = ctx.require_config()?;
    // Use ctx.git, ctx.cargo, ctx.graph as needed
    // ...
    Ok(())
}
```

## Planner Pipeline

`cargo rail plan` is the primary planning surface. The pipeline is deterministic and file-first:

```
1. Collect changed files from git refs
2. Classify builtin file kinds (rust/toml/ci/script/docs) and overlay custom surfaces
3. Resolve file ownership to crates (or workspace/unowned)
4. Expand transitive impact via workspace graph
5. Emit surfaces + trace (proof-carrying reasons)
6. Project an execution scope for runners and CI handoff
```

This output contract is consumed by humans (`--explain`), CI (`-f github`), and executors (`cargo rail run`).

---

## Unify Pipeline

```
cargo metadata (per target, parallel)
         ↓
MultiTargetMetadata (merged view)
         ↓
ManifestAnalyzer (what's used where?)
         ↓
FeatureScanner (classify features)
         ↓
UnifyAnalyzer (generate plan)
         ↓
ManifestWriter (lossless TOML edits)
```

Key insight: operates on **resolved** dependencies (what Cargo chose), not manifest syntax.

---

## Key Design Decisions

| Decision | Why |
|----------|-----|
| System git (no libgit2) | Deterministic SHAs, full git fidelity |
| Resolution-based unification | Accurate to what Cargo actually resolves |
| Lossless TOML (toml_edit) | Preserve comments and formatting |
| Thin main.rs (<100 lines) | All logic testable in library |
| O(1) lookups everywhere | HashMap indexes pre-built at load time |
| Petgraph directly | Own the domain types, no guppy abstraction |

---

## Where to Make Changes

### Adding a new command

1. Define CLI args in `src/commands/cli.rs`
2. Create handler in `src/commands/your_command.rs`
3. Add to dispatch in `src/commands/mod.rs`
4. Handle in `main.rs` if it needs special pre-context handling

### Changing workspace loading

→ `src/workspace/context.rs`

### Modifying dependency graph logic

→ `src/graph/core.rs` (structure)
→ `src/graph/query.rs` (algorithms)

### Adjusting unification

→ `src/cargo/unify_analyzer.rs` (plan generation)
→ `src/cargo/manifest_writer.rs` (TOML output)

### Changing planning behavior

→ `src/commands/plan.rs` (planner pipeline + surfaces + trace)
→ `src/change_detection/classify.rs` (file kind classification)

### Modifying split/sync/release

→ `src/split/engine.rs`
→ `src/sync/engine.rs`
→ `src/release/planner.rs`, `publisher.rs`

---

## File → Module Quick Reference

```
"Where do I find..."

planner contract          → src/commands/plan.rs

run executor              → src/commands/run.rs
                          → src/test/

dependency unification    → src/cargo/unify_*.rs
                          → src/commands/unify.rs

split operation           → src/split/engine.rs
                          → src/commands/split.rs

sync operation            → src/sync/engine.rs
                          → src/commands/sync.rs

release workflow          → src/release/planner.rs
                          → src/release/publisher.rs
                          → src/commands/release.rs

config loading            → src/config/mod.rs

git operations            → src/git/system.rs
                          → src/git/ops.rs

error handling            → src/error.rs

output control            → src/output.rs
```

---

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Check mode found changes (actionable, not error) |
| 2 | Error |

Exit code 1 lets CI detect "changes needed" vs "something broke".
