//! TOML manipulation and formatting.
//!
//! `format` renders and standardizes TOML text; `editor` applies validated, atomic edits to one file.

pub mod editor;
pub mod format;

pub use editor::TomlEditor;
pub use format::TomlFormatter;
