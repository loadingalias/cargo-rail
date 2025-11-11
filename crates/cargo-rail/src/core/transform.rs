#![allow(dead_code)]

use crate::core::error::RailResult;

/// Transform trait for language-agnostic file transformations
pub trait Transform {
  /// Transform file contents when splitting from monorepo to split repo
  fn transform_to_split(&self, content: &str, context: &TransformContext) -> RailResult<String>;

  /// Transform file contents when syncing from split repo to monorepo
  fn transform_to_mono(&self, content: &str, context: &TransformContext) -> RailResult<String>;
}

/// Context provided to transforms
pub struct TransformContext {
  pub crate_name: String,
  pub workspace_root: std::path::PathBuf,
  // Add more context as needed
}
