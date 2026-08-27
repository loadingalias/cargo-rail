//! `cargo rail change` - intent-file management.

use crate::change_detection::classify_path;
use crate::commands::common::ChangeOutputFormat;
use crate::config::{ChangelogConfig, ChangelogFilters, ReleaseConfig, ReleaseSource};
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::release::attribution::{AttributedHistory, CommitAttributor};
use crate::release::change_files::{
    ChangeBump, PendingChangeSet, parse_change_file, render_change_content, write_change_file,
};
use crate::workspace::WorkspaceContext;
use rustc_hash::FxHashSet;
use std::collections::BTreeMap;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Options for `cargo rail change check`.
#[derive(Debug, Clone, Default)]
pub struct ChangeCheckOptions {
    /// Git ref to compare against.
    pub since: Option<String>,
    /// Compare from merge-base with the default branch.
    pub merge_base: bool,
    /// Scan full reachable history.
    pub all: bool,
    /// Require every changed crate to have a change file.
    pub required: bool,
    /// Output format.
    pub format: ChangeOutputFormat,
}

/// Add a pending change file.
pub fn run_change_add(
    ctx: &WorkspaceContext,
    crate_names: Vec<String>,
    bump: String,
    message: Option<String>,
    name: Option<String>,
    format: ChangeOutputFormat,
) -> RailResult<()> {
    ctx.snapshot()?;
    if crate_names.is_empty() {
        return Err(RailError::with_help(
            "change add requires at least one crate",
            "usage: cargo rail change add <CRATE...> --bump <none|patch|minor|major> --message \"reviewed intent\"",
        ));
    }
    let bump = bump.parse::<ChangeBump>()?;

    let workspace_members = ctx.graph().workspace_members();
    for crate_name in &crate_names {
        if !workspace_members.contains(crate_name) {
            return Err(RailError::with_help(
                format!("crate '{}' not found", crate_name),
                format!("available: {}", workspace_members.join(", ")),
            ));
        }
    }

    let change_dir = configured_change_dir(ctx);
    let intents = base_intents(crate_names, bump);
    let file = if let Some(message) = message.map(|msg| msg.trim().to_string()).filter(|msg| !msg.is_empty()) {
        write_change_file(
            ctx.workspace_root(),
            change_dir,
            workspace_members,
            &intents,
            &message,
            name.as_deref(),
        )?
    } else if std::io::stdin().is_terminal() {
        let edited = edit_change_file(ctx.workspace_root(), change_dir, workspace_members, &intents)?;
        write_change_file(
            ctx.workspace_root(),
            change_dir,
            workspace_members,
            &edited.intents,
            &edited.body,
            name.as_deref(),
        )?
    } else {
        return Err(RailError::with_help(
            "change add requires --message in non-interactive mode",
            "pass --message, or run from a terminal with VISUAL or EDITOR set",
        ));
    };

    if format.is_json() {
        let payload = serde_json::json!({
          "action": "add",
          "path": &file.path,
          "crates": file.intents.iter().map(|intent| intent.crate_name.clone()).collect::<Vec<_>>(),
          "bump": file.intents.iter().map(|intent| intent.bump).max().unwrap_or(bump).as_str(),
        });
        let envelope = crate::output::machine_json_envelope("change", "add", "success", 0, payload);
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else if format == ChangeOutputFormat::NamesOnly {
        println!("{}", file.path.display());
    } else {
        println!("Created {}", display_change_path(ctx, &file.path));
        println!(
            "Bump: {}",
            file.intents
                .iter()
                .map(|intent| format!("{} {}", intent.crate_name, intent.bump))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

/// Print pending change-file status.
pub fn run_change_status(ctx: &WorkspaceContext, format: ChangeOutputFormat) -> RailResult<()> {
    ctx.snapshot()?;

    let workspace_members = ctx.graph().workspace_members();
    let pending = PendingChangeSet::load(ctx.workspace_root(), configured_change_dir(ctx), workspace_members)?;
    let summaries = pending.crate_summaries();

    if format.is_json() {
        let files: Vec<_> = pending
            .files
            .iter()
            .map(|file| {
                serde_json::json!({
                  "path": file.path,
                  "intents": file.intents.iter().map(|intent| {
                    serde_json::json!({ "crate": intent.crate_name, "bump": intent.bump })
                  }).collect::<Vec<_>>(),
                  "body": file.body,
                })
            })
            .collect();
        let payload = serde_json::json!({
          "action": "status",
          "files": files,
          "crates": summaries,
          "count": pending.files.len(),
        });
        let envelope = crate::output::machine_json_envelope("change", "status", "success", 0, payload);
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }

    if format == ChangeOutputFormat::NamesOnly {
        for file in &pending.files {
            println!("{}", display_change_path(ctx, &file.path));
        }
        return Ok(());
    }

    if pending.is_empty() {
        println!("no pending change files");
        return Ok(());
    }

    println!("resulting bumps:");
    for summary in &summaries {
        let suffix = if summary.file_count == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", summary.file_count)
        };
        println!("  {}: {} ({})", summary.crate_name, summary.bump, suffix);
    }
    if crate::output::is_verbose() {
        println!();
        println!("pending change files:");
        for file in &pending.files {
            println!("  {}", display_change_path(ctx, &file.path));
            for intent in &file.intents {
                println!("    {}: {}", intent.crate_name, intent.bump);
            }
            if let Some(first_line) = file.body.lines().find(|line| !line.trim().is_empty()) {
                println!("    {}", first_line.trim());
            }
        }
    }
    Ok(())
}

/// Check changed crates for pending change-file coverage.
pub fn run_change_check(ctx: &WorkspaceContext, options: ChangeCheckOptions) -> RailResult<()> {
    ctx.snapshot()?;
    let default_release_config;
    let release_config = if let Some(config) = ctx.config() {
        &config.release
    } else {
        default_release_config = ReleaseConfig::default();
        &default_release_config
    };

    let workspace_members = ctx.graph().workspace_members();
    let base = resolve_change_check_base(ctx, &options)?;
    let pending = PendingChangeSet::load(ctx.workspace_root(), &release_config.change_dir, workspace_members)?;
    let changed_code_crates = changed_code_crates(ctx, release_config, base.as_deref(), workspace_members)?;
    let covered_crates: Vec<_> = changed_code_crates
        .iter()
        .filter(|crate_name| pending.covers(crate_name))
        .cloned()
        .collect();
    let missing_change_files: Vec<_> = changed_code_crates
        .iter()
        .filter(|crate_name| !pending.covers(crate_name))
        .filter(|crate_name| options.required || release_config.requires_change_file(crate_name))
        .cloned()
        .collect();

    match options.format {
        ChangeOutputFormat::Text => print_change_check_text(
            base.as_deref(),
            options.required,
            &changed_code_crates,
            &covered_crates,
            &missing_change_files,
        ),
        ChangeOutputFormat::Json => print_change_check_json(
            base.as_deref(),
            options.required,
            &release_config.change_dir,
            &changed_code_crates,
            &covered_crates,
            &missing_change_files,
        )?,
        ChangeOutputFormat::NamesOnly => {
            for crate_name in &missing_change_files {
                println!("{}", crate_name);
            }
        }
    }

    if missing_change_files.is_empty() {
        Ok(())
    } else if options.format.is_json() {
        Err(RailError::ExitWithCode { code: 1 })
    } else {
        Err(RailError::CheckHasPendingChanges)
    }
}

fn display_change_path(ctx: &WorkspaceContext, path: &Path) -> String {
    let relative = path.strip_prefix(ctx.workspace_root()).unwrap_or(path);
    crate::utils::path_to_git_format(relative)
}

fn resolve_change_check_base(ctx: &WorkspaceContext, options: &ChangeCheckOptions) -> RailResult<Option<String>> {
    if options.all {
        return Ok(None);
    }

    let git = ctx.git()?.git();
    if options.merge_base {
        let default_branch = detect_default_base_ref(git)?;
        return Ok(Some(git.get_merge_base(&default_branch, "HEAD")?));
    }

    if let Some(since) = &options.since {
        return Ok(Some(since.clone()));
    }

    detect_default_base_ref(git).map(Some)
}

fn changed_code_crates(
    ctx: &WorkspaceContext,
    release_config: &ReleaseConfig,
    base: Option<&str>,
    workspace_members: &[String],
) -> RailResult<Vec<String>> {
    if release_config.source == ReleaseSource::Changes {
        let mut changed = FxHashSet::default();
        for repository_path in ctx.source_paths_since(base)? {
            let Some(workspace_path) = ctx.to_workspace_path(&repository_path) else {
                continue;
            };
            if !classify_path(&workspace_path).seeds_build_test_transitive() {
                continue;
            }
            if let Some(owner) = ctx.graph().file_to_crate(&workspace_path) {
                changed.insert(owner);
            }
        }
        let mut changed: Vec<_> = changed.into_iter().collect();
        changed.sort();
        return Ok(changed);
    }

    let attributor = CommitAttributor::new(ctx);
    let mut histories: BTreeMap<HistoryKey, AttributedHistory> = BTreeMap::new();
    let mut changed = Vec::new();

    for crate_name in workspace_members {
        let changelog_config = ctx
            .config()
            .as_ref()
            .and_then(|config| config.crates.get(crate_name))
            .and_then(|crate_config| crate_config.changelog.as_ref());
        let filters = effective_changelog_filters(&release_config.changelog.filters, changelog_config);
        let history = history_for_change_check(&mut histories, &attributor, base, filters)?;
        if history.has_code_changes(crate_name) {
            changed.push(crate_name.clone());
        }
    }

    changed.sort();
    Ok(changed)
}

fn history_for_change_check<'a>(
    histories: &'a mut BTreeMap<HistoryKey, AttributedHistory>,
    attributor: &CommitAttributor<'_>,
    base: Option<&str>,
    filters: &ChangelogFilters,
) -> RailResult<&'a AttributedHistory> {
    let key = HistoryKey::new(base, filters);
    if !histories.contains_key(&key) {
        let history = attributor.history_with_filters(base, "HEAD", Some(filters))?;
        histories.insert(key.clone(), history);
    }

    histories
        .get(&key)
        .ok_or_else(|| RailError::message("internal error: missing cached change-check history"))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HistoryKey {
    base: Option<String>,
    skip_types: Vec<String>,
    skip_scopes: Vec<String>,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
}

impl HistoryKey {
    fn new(base: Option<&str>, filters: &ChangelogFilters) -> Self {
        Self {
            base: base.map(str::to_string),
            skip_types: filters.skip_types.clone(),
            skip_scopes: filters.skip_scopes.clone(),
            include_paths: filters.include_paths.clone(),
            exclude_paths: filters.exclude_paths.clone(),
        }
    }
}

fn effective_changelog_filters<'a>(
    workspace_filters: &'a ChangelogFilters,
    overrides: Option<&'a ChangelogConfig>,
) -> &'a ChangelogFilters {
    overrides
        .and_then(|config| config.filters.as_ref())
        .unwrap_or(workspace_filters)
}

fn print_change_check_text(
    base: Option<&str>,
    required_override: bool,
    changed_code_crates: &[String],
    covered_crates: &[String],
    missing_change_files: &[String],
) {
    if missing_change_files.is_empty() {
        println!("change files: ok");
    } else {
        println!("missing change files:");
        for crate_name in missing_change_files {
            println!("  {}", crate_name);
        }
    }
    if crate::output::is_verbose() {
        println!("Base: {}", base.unwrap_or("all history"));
        println!(
            "Requirement: {}",
            if required_override {
                "all changed crates"
            } else {
                "release.require_change_files"
            }
        );
        if !changed_code_crates.is_empty() {
            println!("Changed code crates: {}", changed_code_crates.join(", "));
        }
        if !covered_crates.is_empty() {
            println!("Covered crates: {}", covered_crates.join(", "));
        }
    }
}

fn print_change_check_json(
    base: Option<&str>,
    required_override: bool,
    change_dir: &str,
    changed_code_crates: &[String],
    covered_crates: &[String],
    missing_change_files: &[String],
) -> RailResult<()> {
    let status = if missing_change_files.is_empty() {
        "success"
    } else {
        "check_failed"
    };
    let exit_code = if missing_change_files.is_empty() { 0 } else { 1 };
    let payload = serde_json::json!({
      "action": "check",
      "base": base,
      "head": "HEAD",
      "required": if required_override { "all" } else { "configured" },
      "change_dir": change_dir,
      "changed_code_crates": changed_code_crates,
      "covered_crates": covered_crates,
      "missing_change_files": missing_change_files,
    });
    let envelope = crate::output::machine_json_envelope("change", "check", status, exit_code, payload);
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
}

struct EditedChange {
    intents: BTreeMap<String, ChangeBump>,
    body: String,
}

fn base_intents(crate_names: Vec<String>, bump: ChangeBump) -> BTreeMap<String, ChangeBump> {
    crate_names.into_iter().map(|crate_name| (crate_name, bump)).collect()
}

fn configured_change_dir(ctx: &WorkspaceContext) -> &str {
    ctx.config()
        .as_ref()
        .map(|config| config.release.change_dir.as_str())
        .unwrap_or(crate::release::change_files::DEFAULT_CHANGE_DIR)
}

fn edit_change_file(
    workspace_root: &Path,
    change_dir: &str,
    workspace_members: &[String],
    intents: &BTreeMap<String, ChangeBump>,
) -> RailResult<EditedChange> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|value| !value.trim().is_empty()))
        .ok_or_else(|| {
            RailError::with_help(
                "change add requires VISUAL or EDITOR when --message is omitted",
                "set EDITOR=vim, or pass --message for non-interactive use",
            )
        })?;
    edit_change_file_with_editor(workspace_root, change_dir, workspace_members, intents, &editor)
}

fn edit_change_file_with_editor(
    workspace_root: &Path,
    change_dir: &str,
    workspace_members: &[String],
    intents: &BTreeMap<String, ChangeBump>,
    editor: &str,
) -> RailResult<EditedChange> {
    let dir = workspace_root.join(change_dir);
    fs::create_dir_all(&dir).map_err(|e| RailError::message(format!("failed to create {}: {}", dir.display(), e)))?;
    let draft_path = unique_draft_path(&dir);
    let template = editor_template(intents);
    fs::write(&draft_path, template)
        .map_err(|e| RailError::message(format!("failed to write {}: {}", draft_path.display(), e)))?;

    let edit_result = run_editor(editor, &draft_path);
    if let Err(err) = edit_result {
        drop(fs::remove_file(&draft_path));
        return Err(err);
    }

    let edited = fs::read_to_string(&draft_path)
        .map_err(|e| RailError::message(format!("failed to read {}: {}", draft_path.display(), e)))?;
    fs::remove_file(&draft_path)
        .map_err(|e| RailError::message(format!("failed to remove {}: {}", draft_path.display(), e)))?;

    let normalized = strip_editor_comments(&edited);
    let member_set: FxHashSet<&str> = workspace_members.iter().map(String::as_str).collect();
    let parsed = parse_change_file(&PathBuf::from("<editor>"), &normalized, &member_set)?;
    let intents = parsed
        .intents
        .into_iter()
        .map(|intent| (intent.crate_name, intent.bump))
        .collect();
    Ok(EditedChange {
        intents,
        body: parsed.body,
    })
}

fn editor_template(intents: &BTreeMap<String, ChangeBump>) -> String {
    let body = "# Write the user-facing changelog entry below.\n# Lines starting with # are ignored.\n";
    render_change_content(intents, body)
}

fn strip_editor_comments(content: &str) -> String {
    let Some(rest) = content.strip_prefix("---\n") else {
        return content.to_string();
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        return content.to_string();
    };

    let body = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    format!("---\n{}\n---\n\n{}\n", frontmatter.trim_end(), body.trim())
}

fn unique_draft_path(dir: &Path) -> PathBuf {
    let mut path = dir.join(format!(".change-edit-{}.md", std::process::id()));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!(".change-edit-{}-{}.md", std::process::id(), n));
        n += 1;
    }
    path
}

fn run_editor(editor: &str, path: &Path) -> RailResult<()> {
    #[cfg(unix)]
    let status = Command::new("sh")
        .arg("-c")
        .arg("$RAIL_EDITOR \"$1\"")
        .arg("cargo-rail-editor")
        .arg(path)
        .env("RAIL_EDITOR", editor)
        .status();

    #[cfg(not(unix))]
    let status = {
        let mut parts = editor.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| RailError::message("EDITOR cannot be empty"))?;
        Command::new(program).args(parts).arg(path).status()
    };

    let status = status.map_err(|e| RailError::message(format!("failed to launch editor '{}': {}", editor, e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(RailError::message(format!(
            "editor '{}' exited with {}",
            editor, status
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_comment_lines_are_removed() {
        let content = "---\n\"rail-core\" = \"minor\"\n---\n\n# comment\nEdited body.\n";
        assert_eq!(
            strip_editor_comments(content),
            "---\n\"rail-core\" = \"minor\"\n---\n\nEdited body.\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn editor_path_parses_edited_body() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::TempDir::new().unwrap();
        let editor = root.path().join("editor.sh");
        fs::write(
            &editor,
            r#"#!/bin/sh
printf '\nEdited body from editor.\n' >> "$1"
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&editor).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&editor, perms).unwrap();

        let mut intents = BTreeMap::new();
        intents.insert("rail-core".to_string(), ChangeBump::Minor);
        let members = vec!["rail-core".to_string()];
        let edited = edit_change_file_with_editor(
            root.path(),
            crate::release::change_files::DEFAULT_CHANGE_DIR,
            &members,
            &intents,
            editor.to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(edited.body, "Edited body from editor.");
        assert_eq!(edited.intents.get("rail-core"), Some(&ChangeBump::Minor));
    }
}
