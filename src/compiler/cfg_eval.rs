//! Exact target cfg expression evaluation using rustc-provided cfg sets.

use crate::error::{RailError, RailResult, ResultExt};
use std::collections::{HashMap, HashSet};
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

#[derive(Debug)]
enum CfgExpr {
  Predicate { key: String, value: Option<String> },
  All(Vec<CfgExpr>),
  Any(Vec<CfgExpr>),
  Not(Box<CfgExpr>),
}

fn eval_expr(expr: &CfgExpr, cfg: &TargetCfgSet) -> bool {
  match expr {
    CfgExpr::Predicate { key, value } => cfg.matches_predicate(key, value.as_deref()),
    CfgExpr::All(items) => items.iter().all(|item| eval_expr(item, cfg)),
    CfgExpr::Any(items) => items.iter().any(|item| eval_expr(item, cfg)),
    CfgExpr::Not(item) => !eval_expr(item, cfg),
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
}
