//! Exact target cfg expression evaluation using rustc-provided cfg sets.

use crate::error::{RailError, RailResult, ResultExt};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

/// Parsed cfg flags and key/value sets for one target triple.
#[derive(Debug, Clone, Default)]
pub struct TargetCfgSet {
  flags: HashSet<String>,
  key_values: HashMap<String, HashSet<String>>,
}

impl TargetCfgSet {
  fn from_rustc_output(output: &str) -> Self {
    let mut set = Self::default();

    for line in output.lines().map(str::trim).filter(|line| !line.is_empty()) {
      if let Some((key, value)) = parse_key_value(line) {
        set.key_values.entry(key).or_default().insert(value);
      } else {
        set.flags.insert(line.to_string());
      }
    }

    set
  }

  fn matches_predicate(&self, key: &str, value: Option<&str>) -> bool {
    match value {
      Some(value) => self.key_values.get(key).is_some_and(|values| values.contains(value)),
      None => self.flags.contains(key),
    }
  }

  fn predicate_applicability(&self, key: &str, value: Option<&str>) -> CfgApplicability {
    if key == "feature" {
      return CfgApplicability::Maybe;
    }
    if self.matches_predicate(key, value) {
      return CfgApplicability::Yes;
    }
    if key.starts_with("target_") || key == "panic" || (value.is_none() && matches!(key, "unix" | "windows")) {
      CfgApplicability::No
    } else {
      // Build scripts and caller-provided rustflags can define custom cfgs that
      // are absent from bare `rustc --print cfg` output.
      CfgApplicability::Maybe
    }
  }

  #[cfg(test)]
  pub(crate) fn from_test_lines(lines: &[&str]) -> Self {
    Self::from_rustc_output(&lines.join("\n"))
  }
}

/// Load `rustc --print cfg` for each configured target.
pub fn load_target_cfg_sets(workspace_root: &Path, targets: &[&str]) -> RailResult<HashMap<String, TargetCfgSet>> {
  let mut by_target = HashMap::with_capacity(targets.len());

  for target in targets {
    let mut cmd = Command::new("rustc");
    cmd.current_dir(workspace_root).args(["--print", "cfg"]);
    if *target != "default" {
      cmd.args(["--target", target]);
    }

    let output = cmd
      .output()
      .with_context(|| format!("running rustc --print cfg for target '{target}'"))?;

    if !output.status.success() {
      return Err(RailError::message(format!(
        "rustc --print cfg failed for target '{target}' with status {}",
        output.status
      )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    by_target.insert((*target).to_string(), TargetCfgSet::from_rustc_output(&stdout));
  }

  Ok(by_target)
}

/// Returns true when `target_constraint` matches the given target triple.
#[must_use]
pub fn target_constraint_matches_target(target_constraint: &str, target: &str, cfg_set: Option<&TargetCfgSet>) -> bool {
  // Explicit triple target section: [target.'x86_64-unknown-linux-gnu'.dependencies]
  if !target_constraint.starts_with("cfg(") {
    return target_constraint == target;
  }

  let Some(inner) = target_constraint.strip_prefix("cfg(").and_then(|s| s.strip_suffix(')')) else {
    return false;
  };

  let Some(cfg_set) = cfg_set else {
    return false;
  };

  let mut parser = CfgParser::new(inner);
  let Ok(expr) = parser.parse_expr() else {
    return false;
  };
  if parser.has_remaining_tokens() {
    return false;
  }

  eval_expr(&expr, cfg_set)
}

/// Return whether a source cfg expression can apply to at least one configured
/// platform, treating feature and unknown custom cfg predicates as unresolved.
///
/// This is deliberately fail-closed for cleanup: malformed expressions,
/// missing cfg sets, and build-script-defined predicates all return `true`.
#[must_use]
pub(crate) fn cfg_expression_may_apply<'a>(
  expression: &str,
  configured_cfgs: impl Iterator<Item = Option<&'a TargetCfgSet>>,
) -> bool {
  let mut parser = CfgParser::new(expression);
  let Ok(parsed) = parser.parse_expr() else {
    return true;
  };
  if parser.has_remaining_tokens() {
    return true;
  }

  configured_cfgs
    .into_iter()
    .any(|cfg| cfg.is_none_or(|cfg| eval_applicability(&parsed, cfg) != CfgApplicability::No))
}

#[derive(Debug, Clone)]
enum CfgExpr {
  Predicate { key: String, value: Option<String> },
  All(Vec<CfgExpr>),
  Any(Vec<CfgExpr>),
  Not(Box<CfgExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CfgApplicability {
  No,
  Maybe,
  Yes,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct FeatureAssignment {
  enabled: BTreeSet<String>,
  disabled: BTreeSet<String>,
}

/// Produce minimal deterministic feature sets that can make a cfg expression true.
///
/// Non-feature predicates are left to the platform matrix. Contradictory
/// feature branches are discarded.
pub(crate) fn feature_selections_for_cfg(expression: &str) -> Vec<Vec<String>> {
  let mut parser = CfgParser::new(expression);
  let Ok(parsed) = parser.parse_expr() else {
    return Vec::new();
  };
  if parser.has_remaining_tokens() {
    return Vec::new();
  }
  let mut assignments = assignments_for(&parsed, true);
  assignments.sort();
  assignments.dedup();
  assignments
    .into_iter()
    .filter(|assignment| assignment.enabled.is_disjoint(&assignment.disabled))
    .map(|assignment| assignment.enabled.into_iter().collect())
    .collect()
}

fn assignments_for(expression: &CfgExpr, desired: bool) -> Vec<FeatureAssignment> {
  match expression {
    CfgExpr::Predicate { key, value } if key == "feature" => {
      let Some(feature) = value else {
        return Vec::new();
      };
      let mut assignment = FeatureAssignment::default();
      if desired {
        assignment.enabled.insert(feature.clone());
      } else {
        assignment.disabled.insert(feature.clone());
      }
      vec![assignment]
    }
    CfgExpr::Predicate { .. } => vec![FeatureAssignment::default()],
    CfgExpr::Not(inner) => assignments_for(inner, !desired),
    CfgExpr::All(items) if desired => combine_assignments(items.iter().map(|item| assignments_for(item, true))),
    CfgExpr::Any(items) if !desired => combine_assignments(items.iter().map(|item| assignments_for(item, false))),
    CfgExpr::All(items) => items.iter().flat_map(|item| assignments_for(item, false)).collect(),
    CfgExpr::Any(items) => items.iter().flat_map(|item| assignments_for(item, true)).collect(),
  }
}

fn combine_assignments(parts: impl Iterator<Item = Vec<FeatureAssignment>>) -> Vec<FeatureAssignment> {
  let mut combined = vec![FeatureAssignment::default()];
  for alternatives in parts {
    let mut next = Vec::new();
    for left in &combined {
      for right in &alternatives {
        let mut merged = left.clone();
        merged.enabled.extend(right.enabled.iter().cloned());
        merged.disabled.extend(right.disabled.iter().cloned());
        if merged.enabled.is_disjoint(&merged.disabled) {
          next.push(merged);
        }
      }
    }
    combined = next;
  }
  combined
}

fn eval_expr(expr: &CfgExpr, cfg: &TargetCfgSet) -> bool {
  match expr {
    CfgExpr::Predicate { key, value } => cfg.matches_predicate(key, value.as_deref()),
    CfgExpr::All(items) => items.iter().all(|item| eval_expr(item, cfg)),
    CfgExpr::Any(items) => items.iter().any(|item| eval_expr(item, cfg)),
    CfgExpr::Not(item) => !eval_expr(item, cfg),
  }
}

fn eval_applicability(expr: &CfgExpr, cfg: &TargetCfgSet) -> CfgApplicability {
  match expr {
    CfgExpr::Predicate { key, value } => cfg.predicate_applicability(key, value.as_deref()),
    CfgExpr::All(items) => {
      let mut result = CfgApplicability::Yes;
      for item in items {
        match eval_applicability(item, cfg) {
          CfgApplicability::No => return CfgApplicability::No,
          CfgApplicability::Maybe => result = CfgApplicability::Maybe,
          CfgApplicability::Yes => {}
        }
      }
      result
    }
    CfgExpr::Any(items) => {
      let mut result = CfgApplicability::No;
      for item in items {
        match eval_applicability(item, cfg) {
          CfgApplicability::Yes => return CfgApplicability::Yes,
          CfgApplicability::Maybe => result = CfgApplicability::Maybe,
          CfgApplicability::No => {}
        }
      }
      result
    }
    CfgExpr::Not(item) => match eval_applicability(item, cfg) {
      CfgApplicability::No => CfgApplicability::Yes,
      CfgApplicability::Maybe => CfgApplicability::Maybe,
      CfgApplicability::Yes => CfgApplicability::No,
    },
  }
}

struct CfgParser<'a> {
  input: &'a [u8],
  pos: usize,
}

impl<'a> CfgParser<'a> {
  fn new(input: &'a str) -> Self {
    Self {
      input: input.as_bytes(),
      pos: 0,
    }
  }

  fn parse_expr(&mut self) -> Result<CfgExpr, ()> {
    self.skip_ws();
    let ident = self.parse_ident()?;
    self.skip_ws();

    if self.consume_char('(') {
      match ident.as_str() {
        "all" => {
          let args = self.parse_expr_list()?;
          self.expect_char(')')?;
          Ok(CfgExpr::All(args))
        }
        "any" => {
          let args = self.parse_expr_list()?;
          self.expect_char(')')?;
          Ok(CfgExpr::Any(args))
        }
        "not" => {
          let expr = self.parse_expr()?;
          self.expect_char(')')?;
          Ok(CfgExpr::Not(Box::new(expr)))
        }
        _ => Err(()),
      }
    } else if self.consume_char('=') {
      self.skip_ws();
      let value = self.parse_string_literal()?;
      Ok(CfgExpr::Predicate {
        key: ident,
        value: Some(value),
      })
    } else {
      Ok(CfgExpr::Predicate {
        key: ident,
        value: None,
      })
    }
  }

  fn parse_expr_list(&mut self) -> Result<Vec<CfgExpr>, ()> {
    let mut items = Vec::new();
    loop {
      self.skip_ws();
      if self.peek_char() == Some(')') {
        break;
      }
      items.push(self.parse_expr()?);
      self.skip_ws();
      if !self.consume_char(',') {
        break;
      }
    }
    Ok(items)
  }

  fn parse_ident(&mut self) -> Result<String, ()> {
    self.skip_ws();
    let start = self.pos;
    while let Some(c) = self.peek_char() {
      if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
        self.pos += 1;
      } else {
        break;
      }
    }
    if self.pos == start {
      return Err(());
    }
    std::str::from_utf8(&self.input[start..self.pos])
      .map(str::to_string)
      .map_err(|_| ())
  }

  fn parse_string_literal(&mut self) -> Result<String, ()> {
    self.skip_ws();
    self.expect_char('"')?;
    let start = self.pos;
    while let Some(c) = self.peek_char() {
      if c == '"' {
        let value = std::str::from_utf8(&self.input[start..self.pos])
          .map(str::to_string)
          .map_err(|_| ())?;
        self.pos += 1;
        return Ok(value);
      }
      self.pos += 1;
    }
    Err(())
  }

  fn skip_ws(&mut self) {
    while let Some(c) = self.peek_char() {
      if c.is_ascii_whitespace() {
        self.pos += 1;
      } else {
        break;
      }
    }
  }

  fn expect_char(&mut self, expected: char) -> Result<(), ()> {
    if self.consume_char(expected) { Ok(()) } else { Err(()) }
  }

  fn consume_char(&mut self, expected: char) -> bool {
    self.skip_ws();
    if self.peek_char() == Some(expected) {
      self.pos += 1;
      true
    } else {
      false
    }
  }

  fn peek_char(&self) -> Option<char> {
    self.input.get(self.pos).copied().map(char::from)
  }

  fn has_remaining_tokens(&mut self) -> bool {
    self.skip_ws();
    self.pos < self.input.len()
  }
}

fn parse_key_value(line: &str) -> Option<(String, String)> {
  let (key, value_with_quotes) = line.split_once('=')?;
  let key = key.trim();
  let value = value_with_quotes.trim();
  if !(value.starts_with('"') && value.ends_with('"')) {
    return None;
  }
  Some((
    key.to_string(),
    value.trim_start_matches('"').trim_end_matches('"').to_string(),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_cfg_eval_basic_predicates() {
    let cfg = TargetCfgSet::from_rustc_output("unix\ntarget_os=\"linux\"\ntarget_arch=\"x86_64\"\n");

    assert!(target_constraint_matches_target(
      "cfg(unix)",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
    assert!(target_constraint_matches_target(
      "cfg(target_os = \"linux\")",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
    assert!(!target_constraint_matches_target(
      "cfg(target_os = \"windows\")",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
  }

  #[test]
  fn test_cfg_eval_any_all_not() {
    let cfg = TargetCfgSet::from_rustc_output("unix\ntarget_os=\"linux\"\n");

    assert!(target_constraint_matches_target(
      "cfg(any(windows, unix))",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
    assert!(target_constraint_matches_target(
      "cfg(all(unix, target_os = \"linux\"))",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
    assert!(target_constraint_matches_target(
      "cfg(not(windows))",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
  }

  #[test]
  fn test_exact_target_constraint() {
    let cfg = TargetCfgSet::default();
    assert!(target_constraint_matches_target(
      "x86_64-unknown-linux-gnu",
      "x86_64-unknown-linux-gnu",
      Some(&cfg)
    ));
    assert!(!target_constraint_matches_target(
      "x86_64-unknown-linux-gnu",
      "aarch64-apple-darwin",
      Some(&cfg)
    ));
  }

  #[test]
  fn test_feature_selections_cover_compound_positive_and_negative_conditions() {
    assert_eq!(
      feature_selections_for_cfg(r#"all(feature = "a", feature = "b", not(feature = "c"), target_arch = "x86_64")"#),
      vec![vec!["a".to_string(), "b".to_string()]]
    );
  }

  #[test]
  fn test_feature_selections_choose_each_any_branch_without_unrelated_powerset() {
    assert_eq!(
      feature_selections_for_cfg(r#"any(feature = "z", feature = "a")"#),
      vec![vec!["a".to_string()], vec!["z".to_string()]]
    );
  }

  #[test]
  fn test_feature_selections_discard_contradictory_branch() {
    assert!(feature_selections_for_cfg(r#"all(feature = "a", not(feature = "a"))"#).is_empty());
  }

  #[test]
  fn test_cfg_expression_applicability_is_exact_for_platform_and_conservative_for_custom_cfg() {
    let linux = TargetCfgSet::from_test_lines(&["unix", "target_os=\"linux\""]);
    assert!(cfg_expression_may_apply(
      r#"all(target_os = "linux", feature = "api")"#,
      [Some(&linux)].into_iter()
    ));
    assert!(!cfg_expression_may_apply(
      r#"all(target_os = "windows", feature = "api")"#,
      [Some(&linux)].into_iter()
    ));
    assert!(cfg_expression_may_apply(
      r#"all(generated_backend, feature = "api")"#,
      [Some(&linux)].into_iter()
    ));
  }
}
