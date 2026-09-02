//! Conservative Rust module reachability for feature-bound planner roots.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use cargo_metadata::Package;

use crate::compiler::cfg_eval::cfg_expression_feature_match;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceActivation {
    Active,
    Inactive,
    Unknown,
}

/// Expand exact root features through Cargo's member-local feature graph.
pub(super) fn expanded_features(package: &Package, roots: &[String]) -> RailResult<BTreeSet<String>> {
    for feature in roots {
        if feature.is_empty() || feature.starts_with('-') {
            return Err(RailError::message(format!(
                "feature root for package '{}' is empty or option-like",
                package.name
            )));
        }
        if !package.features.contains_key(feature) {
            return Err(RailError::message(format!(
                "package '{}' does not declare feature '{}'",
                package.name, feature
            )));
        }
    }

    let mut enabled = BTreeSet::new();
    let mut pending = roots.iter().cloned().collect::<VecDeque<_>>();
    while let Some(feature) = pending.pop_front() {
        if !enabled.insert(feature.clone()) {
            continue;
        }
        for edge in package.features.get(&feature).into_iter().flatten() {
            if package.features.contains_key(edge) && !enabled.contains(edge) {
                pending.push_back(edge.clone());
            }
        }
    }
    Ok(enabled)
}

/// Decide whether one captured Rust path is reachable for an exact feature set.
///
/// This recognizes ordinary external modules emitted by rustfmt. Rust forms
/// whose path or cfg semantics cannot be proved here deliberately return
/// [`SourceActivation::Unknown`], which makes the planner widen.
pub(super) fn source_activation(
    ctx: &WorkspaceContext,
    package: &Package,
    selected_target: Option<(&str, &[String])>,
    path: &str,
    enabled_features: &BTreeSet<String>,
) -> RailResult<SourceActivation> {
    let changed = Path::new(path);
    if changed.extension().and_then(std::ffi::OsStr::to_str) != Some("rs") {
        return Ok(SourceActivation::Unknown);
    }

    let mut saw_inactive = false;
    let mut saw_unknown = false;
    for target in package.targets.iter().filter(|target| {
        selected_target.is_none_or(|(name, kinds)| {
            target.name == name && target.kind.iter().any(|kind| kinds.contains(&kind.to_string()))
        })
    }) {
        if !target
            .required_features
            .iter()
            .all(|feature| enabled_features.contains(feature))
        {
            saw_inactive = true;
            continue;
        }
        let Ok(root) = target
            .src_path
            .as_std_path()
            .strip_prefix(ctx.planning_authority_source_root())
        else {
            saw_unknown = true;
            continue;
        };
        match trace_module_path(ctx, root, changed, enabled_features)? {
            SourceActivation::Active => return Ok(SourceActivation::Active),
            SourceActivation::Inactive => saw_inactive = true,
            SourceActivation::Unknown => saw_unknown = true,
        }
    }

    Ok(if saw_unknown {
        SourceActivation::Unknown
    } else if saw_inactive {
        SourceActivation::Inactive
    } else {
        SourceActivation::Unknown
    })
}

fn trace_module_path(
    ctx: &WorkspaceContext,
    root: &Path,
    changed: &Path,
    enabled_features: &BTreeSet<String>,
) -> RailResult<SourceActivation> {
    if root == changed {
        return Ok(SourceActivation::Active);
    }
    let Some(root_dir) = root.parent() else {
        return Ok(SourceActivation::Unknown);
    };
    if !changed.starts_with(root_dir) {
        return Ok(SourceActivation::Unknown);
    }

    let mut current = root.to_path_buf();
    let mut uncertain = false;
    loop {
        let search_dir = module_search_dir(root, &current)?;
        let Ok(suffix) = changed.strip_prefix(&search_dir) else {
            return Ok(SourceActivation::Unknown);
        };
        let Some(component) = suffix.components().next() else {
            return Ok(SourceActivation::Unknown);
        };
        let module_name = component.as_os_str().to_string_lossy();
        let module = module_name.strip_suffix(".rs").unwrap_or_else(|| module_name.as_ref());
        if module.is_empty() || module == "mod" {
            return Ok(SourceActivation::Unknown);
        }

        let bytes = read_planning_file(ctx, &current)?;
        let Ok(source) = std::str::from_utf8(&bytes) else {
            return Ok(SourceActivation::Unknown);
        };
        let Some(conditions) = module_conditions(source, module) else {
            return Ok(SourceActivation::Unknown);
        };
        for condition in &conditions {
            match cfg_expression_feature_match(condition, enabled_features) {
                Some(true) => {}
                Some(false) => return Ok(SourceActivation::Inactive),
                None => uncertain = true,
            }
        }

        let flat = search_dir.join(format!("{module}.rs"));
        let nested = search_dir.join(module).join("mod.rs");
        current = if changed == flat {
            flat
        } else if changed == nested {
            nested
        } else if changed.starts_with(search_dir.join(module)) {
            let Some(next) = choose_existing_module(ctx, &flat, &nested)? else {
                return Ok(SourceActivation::Unknown);
            };
            next
        } else {
            return Ok(SourceActivation::Unknown);
        };
        if current == changed {
            return Ok(if uncertain {
                SourceActivation::Unknown
            } else {
                SourceActivation::Active
            });
        }
    }
}

fn module_search_dir(root: &Path, current: &Path) -> RailResult<PathBuf> {
    let parent = current
        .parent()
        .ok_or_else(|| RailError::message(format!("Rust module '{}' has no parent", current.display())))?;
    if current == root || current.file_name().is_some_and(|name| name == "mod.rs") {
        return Ok(parent.to_path_buf());
    }
    let stem = current
        .file_stem()
        .ok_or_else(|| RailError::message(format!("Rust module '{}' has no file stem", current.display())))?;
    Ok(parent.join(stem))
}

fn choose_existing_module(ctx: &WorkspaceContext, flat: &Path, nested: &Path) -> RailResult<Option<PathBuf>> {
    let flat_bytes = read_planning_file(ctx, flat)?;
    let nested_bytes = read_planning_file(ctx, nested)?;
    match (!flat_bytes.is_empty(), !nested_bytes.is_empty()) {
        (true, false) => Ok(Some(flat.to_path_buf())),
        (false, true) => Ok(Some(nested.to_path_buf())),
        _ => Ok(None),
    }
}

fn read_planning_file(ctx: &WorkspaceContext, path: &Path) -> RailResult<Vec<u8>> {
    let path = crate::utils::path_to_git_format(path);
    ctx.read_planning_current_files(&[path])
        .map(|mut files| files.pop().unwrap_or_default())
}

fn module_conditions(source: &str, expected: &str) -> Option<Vec<String>> {
    let source = strip_block_comments(source)?;
    let mut attributes = Vec::new();
    let mut pending = String::new();
    let mut attribute_depth = 0isize;
    let mut matches = Vec::new();

    for raw in source.lines() {
        let line = raw.split_once("//").map_or(raw, |(before, _)| before).trim();
        if line.is_empty() {
            continue;
        }
        if attribute_depth > 0 || line.starts_with("#[") {
            if !pending.is_empty() {
                pending.push(' ');
            }
            pending.push_str(line);
            attribute_depth += bracket_delta(line);
            if attribute_depth < 0 {
                return None;
            }
            if attribute_depth == 0 {
                attributes.push(std::mem::take(&mut pending));
            }
            continue;
        }
        if !pending.is_empty() {
            return None;
        }

        if let Some(kind) = module_declaration(line, expected) {
            if kind != ';' {
                return None;
            }
            let mut conditions = Vec::new();
            for attribute in &attributes {
                let compact = attribute
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>();
                if compact.starts_with("#[cfg_attr(") || compact.starts_with("#[path=") {
                    return None;
                }
                if let Some(expression) = compact
                    .strip_prefix("#[cfg(")
                    .and_then(|value| value.strip_suffix(")]"))
                {
                    conditions.push(expression.to_string());
                }
            }
            matches.push(conditions);
        }
        attributes.clear();
    }
    (matches.len() == 1).then(|| matches.pop().unwrap_or_default())
}

fn module_declaration(line: &str, expected: &str) -> Option<char> {
    let marker = format!("mod {expected}");
    let start = line.find(&marker)?;
    let before = line.get(..start)?;
    if !before.trim().is_empty() && !before.trim().starts_with("pub") && !before.trim().starts_with("unsafe") {
        return None;
    }
    let after = line.get(start..)?.strip_prefix(&marker)?.trim_start();
    let delimiter = after.chars().next()?;
    matches!(delimiter, ';' | '{').then_some(delimiter)
}

fn bracket_delta(value: &str) -> isize {
    value
        .bytes()
        .map(|byte| match byte {
            b'[' => 1,
            b']' => -1,
            _ => 0,
        })
        .sum()
}

fn strip_block_comments(source: &str) -> Option<String> {
    let mut output = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0usize;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"/*") {
            if depth == 0 {
                output.push(' ');
            }
            depth += 1;
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                output.push(' ');
            }
            index += 2;
        } else {
            if depth == 0 {
                output.push(char::from(bytes[index]));
            } else if bytes[index] == b'\n' {
                output.push('\n');
            }
            index += 1;
        }
    }
    (depth == 0).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_attributes_keep_exact_feature_conditions() {
        let source = r#"
            #[cfg(any(feature = "rsa", feature = "ecdsa"))]
            pub mod signatures;
        "#;
        assert_eq!(
            module_conditions(source, "signatures"),
            Some(vec!["any(feature=\"rsa\",feature=\"ecdsa\")".to_string()])
        );
    }

    #[test]
    fn ambiguous_or_path_overridden_modules_are_unknown() {
        assert_eq!(module_conditions("mod item;\nmod item;", "item"), None);
        assert_eq!(module_conditions("#[path = \"other.rs\"]\nmod item;", "item"), None);
        assert_eq!(module_conditions("m/* token boundary */od item;", "item"), None);
    }
}
