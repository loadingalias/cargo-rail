//! Quality analysis engine with trait-based extensibility
//!
//! Single graph build, multiple analyses. All analyses are compiled into the binary.

use crate::core::config::RailConfig;
use crate::core::context::WorkspaceContext;
use crate::core::error::RailResult;
use crate::graph::workspace_graph::WorkspaceGraph;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Severity level for quality violations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
  /// Informational (not a violation)
  Info,
  /// Warning (should fix, not blocking)
  Warning,
  /// Error (must fix)
  Error,
}

/// Result from a single quality analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
  /// Name of the analysis
  pub analysis: String,
  /// Whether the analysis passed (no violations)
  pub passed: bool,
  /// List of violations found
  pub violations: Vec<Violation>,
  /// Summary statistics
  #[serde(skip_serializing_if = "Option::is_none")]
  pub summary: Option<serde_json::Value>,
}

/// A single quality violation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
  /// Severity of this violation
  pub severity: Severity,
  /// Location (crate, file, or workspace-wide)
  pub location: String,
  /// Human-readable description
  pub message: String,
  /// Optional suggested fix
  #[serde(skip_serializing_if = "Option::is_none")]
  pub suggestion: Option<String>,
  /// Additional metadata for tooling
  #[serde(skip_serializing_if = "Option::is_none")]
  pub metadata: Option<serde_json::Value>,
}

impl Violation {
  /// Create an error-level violation
  /// TODO: Used by future quality analyses
  #[allow(dead_code)]
  pub fn error(location: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      severity: Severity::Error,
      location: location.into(),
      message: message.into(),
      suggestion: None,
      metadata: None,
    }
  }

  /// Create a warning-level violation
  /// TODO: Used by future quality analyses
  #[allow(dead_code)]
  pub fn warning(location: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      severity: Severity::Warning,
      location: location.into(),
      message: message.into(),
      suggestion: None,
      metadata: None,
    }
  }

  /// Add a suggestion to this violation
  /// TODO: Used by future quality analyses
  #[allow(dead_code)]
  pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
    self.suggestion = Some(suggestion.into());
    self
  }

  /// Add metadata to this violation
  #[allow(dead_code)]
  pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
    self.metadata = Some(metadata);
    self
  }
}

/// Shared context for quality analyses
///
/// Built once, passed to all analyses. Immutable during analysis phase.
pub struct QualityContext<'a> {
  /// Workspace graph (dependency analysis, crate relationships)
  pub graph: &'a WorkspaceGraph,
  /// Rail configuration (policies, releases, etc.)
  pub config: &'a RailConfig,
  /// Workspace root directory
  /// TODO: Used by future quality analyses that need to read files
  #[allow(dead_code)]
  pub workspace_root: &'a std::path::Path,
}

impl<'a> QualityContext<'a> {
  /// Create a new quality context from workspace context
  pub fn new(workspace_ctx: &'a WorkspaceContext, graph: &'a WorkspaceGraph, config: &'a RailConfig) -> Self {
    Self {
      graph,
      config,
      workspace_root: workspace_ctx.workspace_root(),
    }
  }
}

/// Quality analysis trait
///
/// All analyses implement this trait. Analyses are stateless and can run
/// concurrently over the shared context.
pub trait QualityAnalysis: Send + Sync {
  /// Unique name for this analysis (kebab-case)
  fn name(&self) -> &str;

  /// Human-readable description
  fn description(&self) -> &str;

  /// Run the analysis and return violations
  fn analyze(&self, ctx: &QualityContext) -> RailResult<AnalysisResult>;

  /// Whether this analysis can auto-fix violations
  fn supports_autofix(&self) -> bool {
    false
  }

  /// Apply auto-fixes for violations (if supported)
  ///
  /// Only called if supports_autofix() returns true.
  fn apply_fixes(&self, _ctx: &QualityContext, _violations: &[Violation]) -> RailResult<usize> {
    Ok(0)
  }
}

/// Quality analysis engine
///
/// Orchestrates running multiple analyses over a shared context.
pub struct QualityEngine {
  analyses: Vec<Arc<dyn QualityAnalysis>>,
}

impl QualityEngine {
  /// Create a new empty engine
  pub fn new() -> Self {
    Self { analyses: Vec::new() }
  }

  /// Register an analysis with the engine
  pub fn register(&mut self, analysis: Arc<dyn QualityAnalysis>) {
    self.analyses.push(analysis);
  }

  /// Run all registered analyses
  pub fn run_all(&self, ctx: &QualityContext) -> RailResult<QualityReport> {
    let mut results = Vec::new();

    for analysis in &self.analyses {
      let result = analysis.analyze(ctx)?;
      results.push(result);
    }

    Ok(QualityReport { results })
  }

  /// Run a specific analysis by name
  pub fn run_one(&self, ctx: &QualityContext, name: &str) -> RailResult<Option<AnalysisResult>> {
    for analysis in &self.analyses {
      if analysis.name() == name {
        return Ok(Some(analysis.analyze(ctx)?));
      }
    }
    Ok(None)
  }

  /// Get all registered analyses
  pub fn analyses(&self) -> &[Arc<dyn QualityAnalysis>] {
    &self.analyses
  }

  /// Apply auto-fixes for a specific analysis
  pub fn apply_fixes(&self, ctx: &QualityContext, analysis_name: &str) -> RailResult<usize> {
    for analysis in &self.analyses {
      if analysis.name() == analysis_name {
        if !analysis.supports_autofix() {
          return Ok(0);
        }

        // Run analysis to get violations
        let result = analysis.analyze(ctx)?;
        let fixable: Vec<_> = result.violations.to_vec();

        return analysis.apply_fixes(ctx, &fixable);
      }
    }
    Ok(0)
  }
}

impl Default for QualityEngine {
  fn default() -> Self {
    Self::new()
  }
}

/// Unified quality report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
  /// Results from all analyses
  pub results: Vec<AnalysisResult>,
}

impl QualityReport {
  /// Check if all analyses passed
  pub fn passed(&self) -> bool {
    self.results.iter().all(|r| r.passed)
  }

  /// Count total violations by severity
  pub fn count_violations(&self) -> (usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut info = 0;

    for result in &self.results {
      for violation in &result.violations {
        match violation.severity {
          Severity::Error => errors += 1,
          Severity::Warning => warnings += 1,
          Severity::Info => info += 1,
        }
      }
    }

    (errors, warnings, info)
  }

  /// Get all violations across all analyses
  /// TODO: Used by future quality report formatters
  #[allow(dead_code)]
  pub fn all_violations(&self) -> Vec<&Violation> {
    self.results.iter().flat_map(|r| &r.violations).collect()
  }

  /// Convert to JSON
  pub fn to_json(&self) -> RailResult<String> {
    serde_json::to_string_pretty(self)
      .map_err(|e| crate::core::error::RailError::message(format!("JSON serialization failed: {}", e)))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct MockAnalysis {
    name: &'static str,
    should_fail: bool,
  }

  impl QualityAnalysis for MockAnalysis {
    fn name(&self) -> &str {
      self.name
    }

    fn description(&self) -> &str {
      "Mock analysis for testing"
    }

    fn analyze(&self, _ctx: &QualityContext) -> RailResult<AnalysisResult> {
      let violations = if self.should_fail {
        vec![Violation::error("test-crate", "Mock violation")]
      } else {
        vec![]
      };

      Ok(AnalysisResult {
        analysis: self.name.to_string(),
        passed: violations.is_empty(),
        violations,
        summary: None,
      })
    }
  }

  #[test]
  fn test_engine_creation() {
    let engine = QualityEngine::new();
    assert_eq!(engine.analyses().len(), 0);
  }

  #[test]
  fn test_engine_registration() {
    let mut engine = QualityEngine::new();
    engine.register(Arc::new(MockAnalysis {
      name: "test",
      should_fail: false,
    }));
    assert_eq!(engine.analyses().len(), 1);
  }

  #[test]
  fn test_violation_builder() {
    let v = Violation::error("my-crate", "Something is wrong").with_suggestion("Fix it this way");

    assert_eq!(v.severity, Severity::Error);
    assert_eq!(v.location, "my-crate");
    assert_eq!(v.message, "Something is wrong");
    assert_eq!(v.suggestion, Some("Fix it this way".to_string()));
  }

  #[test]
  fn test_report_passed() {
    let report = QualityReport {
      results: vec![AnalysisResult {
        analysis: "test".to_string(),
        passed: true,
        violations: vec![],
        summary: None,
      }],
    };

    assert!(report.passed());
  }

  #[test]
  fn test_report_failed() {
    let report = QualityReport {
      results: vec![AnalysisResult {
        analysis: "test".to_string(),
        passed: false,
        violations: vec![Violation::error("test", "Failed")],
        summary: None,
      }],
    };

    assert!(!report.passed());
    let (errors, warnings, _) = report.count_violations();
    assert_eq!(errors, 1);
    assert_eq!(warnings, 0);
  }
}
