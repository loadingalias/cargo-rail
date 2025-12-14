//! TOML manipulation and formatting library
//!
//! A 3-layer architecture for safe, consistent TOML operations:
//! 1. **Format**: String template building with validation
//! 2. **Builder**: High-level config construction (internal)
//! 3. **Editor**: Safe mutation operations with atomic writes

pub(crate) mod builder; // Internal - used by init command
pub mod editor;
pub mod format;

// Re-exports
#[doc(hidden)]
pub use builder::{RailConfigBuilder, WorkspaceDepsBuilder};
pub use editor::TomlEditor;
pub use format::TomlFormatter;
