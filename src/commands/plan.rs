//! `cargo rail plan` comparison validation and v8 rendering.

use std::io::Write as _;
use std::path::PathBuf;

use crate::error::{RailError, RailResult};
use crate::graph::DependencyUniverse;
use crate::utils::toolchain_fingerprint;
use crate::workspace::WorkspaceContext;

/// Options for the `plan` command.
#[derive(Debug)]
pub struct PlanOptions {
    /// Validated comparison selected before workspace capture.
    pub(crate) comparison: PlanComparison,
    /// Emit the versioned JSON contract instead of text.
    pub json: bool,
    /// Show human evidence detail in text output.
    pub explain: bool,
    /// Monotonically require every registered work item.
    pub all: bool,
    /// Optional portable observed-input evidence.
    pub evidence: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) enum PlanComparison {
    DefaultMergeBase,
    Since(String),
    Objects { from: String, to: String },
}

#[derive(Debug)]
enum ResolvedComparison {
    Worktree { base: String },
    Objects { from: String, to: String },
}

impl PlanComparison {
    pub(crate) fn from_cli(
        since: &Option<String>,
        from: &Option<String>,
        to: &Option<String>,
        merge_base: bool,
    ) -> RailResult<Self> {
        match (from, to) {
            (Some(from), Some(to)) => {
                if since.is_some() || merge_base {
                    return Err(RailError::message(
                        "--from/--to cannot be combined with --since or --merge-base",
                    ));
                }
                return Ok(Self::Objects {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
            (Some(_), None) => return Err(RailError::message("--from requires --to")),
            (None, Some(_)) => return Err(RailError::message("--to requires --from")),
            (None, None) => {}
        }
        Ok(since
            .as_ref()
            .map_or(Self::DefaultMergeBase, |since| Self::Since(since.clone())))
    }

    fn resolve(self, ctx: &WorkspaceContext) -> RailResult<ResolvedComparison> {
        match self {
            Self::Objects { from, to } => Ok(ResolvedComparison::Objects { from, to }),
            Self::Since(base) => Ok(ResolvedComparison::Worktree { base }),
            Self::DefaultMergeBase => {
                let git = ctx.git()?.git();
                let default_branch = crate::git::detect_default_base_ref(git)?;
                Ok(ResolvedComparison::Worktree {
                    base: git.get_merge_base(&default_branch, "HEAD")?,
                })
            }
        }
    }
}

impl ResolvedComparison {
    fn base(&self) -> &str {
        match self {
            Self::Worktree { base } | Self::Objects { from: base, .. } => base,
        }
    }

    fn object_head(&self) -> Option<&str> {
        match self {
            Self::Objects { to, .. } => Some(to),
            Self::Worktree { .. } => None,
        }
    }
}

/// Print the JSON Schema for the current planner contract.
pub fn print_plan_schema() {
    print!("{}", include_str!("../../schemas/plan-v8.schema.json"));
}

/// Build and render one evidence-backed named-work plan.
pub fn run_plan(ctx: &WorkspaceContext, opts: PlanOptions) -> RailResult<()> {
    if opts.json {
        crate::output::set_json_mode(true);
    }
    let plan = build_work_plan(ctx, &opts)?;
    let rendered = if opts.json {
        serde_json::to_string_pretty(&plan)
            .map_err(|error| RailError::message(format!("JSON serialization failed: {error}")))?
    } else {
        crate::planning::format_work_plan(&plan, opts.explain)
    };
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    stdout.write_all(rendered.as_bytes())?;
    if !rendered.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;
    Ok(())
}

fn build_work_plan(ctx: &WorkspaceContext, opts: &PlanOptions) -> RailResult<crate::planning::WorkPlan> {
    let comparison = opts.comparison.clone().resolve(ctx)?;
    let dependency_universe = DependencyUniverse::from_metadata(ctx.cargo().metadata(), ctx.git()?.repo_root())?;
    let planning_index = collect_planning_index(ctx, &comparison)?;
    let semantic_changes = crate::change_detection::semantic::analyze(
        ctx,
        planning_index.files(),
        comparison.base(),
        comparison.object_head(),
    )?;
    let base = ctx.git()?.git().resolve_reference(comparison.base())?;
    let head_commit = comparison.object_head().map_or_else(
        || {
            ctx.planning_head_commit()
                .map(str::to_string)
                .map_or_else(|| ctx.git()?.git().head_commit(), Ok)
        },
        |head| ctx.git()?.git().resolve_reference(head),
    )?;
    let cargo_configuration_identity = ctx.planning_cargo_configuration_identity()?;
    let toolchain_identity = planning_toolchain_identity(ctx)?;
    let authority = crate::planning::WorkPlanAuthority {
        base,
        head: comparison
            .object_head()
            .map_or_else(|| "WORKTREE".to_string(), |_| head_commit.clone()),
        head_commit,
        capture: ctx
            .snapshot_id()
            .map(|snapshot| snapshot.to_string())
            .or_else(|| ctx.planning_snapshot_id()),
        target_identity: planning_target_identity(&cargo_configuration_identity, &toolchain_identity),
        cargo_configuration_identity,
        toolchain_identity,
    };
    crate::planning::build_work_plan(
        ctx,
        &planning_index,
        authority,
        &dependency_universe,
        &semantic_changes,
        opts.evidence.as_deref(),
        opts.all,
    )
}

fn collect_planning_index(
    ctx: &WorkspaceContext,
    comparison: &ResolvedComparison,
) -> RailResult<crate::planning::PlanningIndex> {
    match comparison {
        ResolvedComparison::Objects { from, to } => {
            crate::planning::PlanningIndex::from_objects(ctx, ctx.object_source_changes(from, to)?, from, to)
        }
        ResolvedComparison::Worktree { base } => {
            let changes = if let Some(capture) = ctx.planning_source_capture() {
                capture.changes_from(ctx.git()?.git(), base)?
            } else if let Some(capture) = ctx.source_capture() {
                capture.changes_from(ctx.git()?.git(), base)?
            } else {
                ctx.capture_worktree_source()?.changes_from(ctx.git()?.git(), base)?
            };
            crate::planning::PlanningIndex::from_worktree(ctx, changes, base)
        }
    }
}

fn planning_toolchain_identity(ctx: &WorkspaceContext) -> RailResult<String> {
    if ctx.snapshot_id().is_some() {
        Ok(ctx.snapshot()?.toolchain_fingerprint().to_string())
    } else {
        Ok(toolchain_fingerprint(ctx.workspace_root()))
    }
}

fn planning_target_identity(configuration: &str, toolchain: &str) -> String {
    let input = format!(
        "planning-target-v1\0{}\0{}\0{}\0{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        configuration,
        toolchain
    );
    format!(
        "planning-target-v1:sha256:{}",
        crate::source::ContentDigest::sha256(input.as_bytes())
    )
}
