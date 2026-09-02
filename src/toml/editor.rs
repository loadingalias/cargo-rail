//! Safe TOML file mutation operations

use crate::error::{RailError, RailResult};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Safe TOML file editor with validation and atomic writes
#[derive(Debug)]
pub struct TomlEditor {
    path: PathBuf,
    doc: DocumentMut,
    original_content: String,
}

impl TomlEditor {
    /// Open TOML file for editing
    pub fn open(path: &Path) -> RailResult<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| RailError::message(format!("Failed to read TOML file {}: {}", path.display(), e)))?;

        let doc = content
            .parse::<DocumentMut>()
            .map_err(|e| RailError::message(format!("Failed to parse TOML file {}: {}", path.display(), e)))?;

        Ok(Self {
            path: path.to_path_buf(),
            doc,
            original_content: content,
        })
    }

    /// Get reference to the document
    pub fn doc(&self) -> &DocumentMut {
        &self.doc
    }

    /// Get mutable reference to the document
    pub fn doc_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    /// Set value at path (e.g., "package.version")
    pub fn set(&mut self, path: &str, value: impl Into<Value>) -> RailResult<()> {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return Err(RailError::message("Empty path provided"));
        }

        let mut current = self.doc.as_item_mut();

        // Navigate to parent table
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Last part - set value
                if let Some(table) = current.as_table_mut() {
                    table.insert(part, Item::Value(value.into()));
                } else if let Some(table) = current.as_inline_table_mut() {
                    table.insert(*part, value.into());
                } else {
                    return Err(RailError::message(format!(
                        "cannot set value at path '{}': parent is not a table",
                        path
                    )));
                }
                return Ok(());
            } else {
                // Navigate down
                if let Some(table) = current.as_table_mut() {
                    current = table.entry(part).or_insert(Item::Table(Table::new()));
                } else {
                    return Err(RailError::message(format!(
                        "cannot navigate path '{}': '{}' is not a table",
                        path, part
                    )));
                }
            }
        }

        Ok(())
    }

    /// Get value at path
    pub fn get(&self, path: &str) -> Option<&Item> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = self.doc.as_item();

        for part in parts {
            // Inline tables expose values rather than items, so dotted navigation only
            // traverses standard tables.
            let table = current.as_table()?;
            current = table.get(part)?;
        }

        Some(current)
    }

    /// Check if a key exists at the given path (e.g., "unify.msrv_policy")
    pub fn contains_path(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    /// Remove an existing key at a dotted table path.
    ///
    /// Returns `true` only when a key was removed. Empty parent tables are
    /// removed as well; unrelated formatting is left untouched.
    pub fn remove(&mut self, path: &str) -> bool {
        fn remove_from(table: &mut Table, parts: &[&str]) -> bool {
            let Some((part, remaining)) = parts.split_first() else {
                return false;
            };
            if remaining.is_empty() {
                return table.remove(part).is_some();
            }

            let removed = table
                .get_mut(part)
                .and_then(Item::as_table_mut)
                .is_some_and(|child| remove_from(child, remaining));
            if removed && table.get(part).and_then(Item::as_table).is_some_and(Table::is_empty) {
                table.remove(part);
            }
            removed
        }

        remove_from(self.doc.as_table_mut(), &path.split('.').collect::<Vec<_>>())
    }

    /// Ensure a section (table) exists, creating if needed.
    /// Returns true if the section was created, false if it already existed.
    pub fn ensure_section(&mut self, section: &str) -> bool {
        let mut created = false;
        let mut current = self.doc.as_item_mut();

        for part in section.split('.') {
            let Some(table) = current.as_table_mut() else {
                return created;
            };

            if !table.contains_key(part) {
                table.insert(part, Item::Table(Table::new()));
                created = true;
            }

            let Some(next) = table.get_mut(part) else {
                return created;
            };
            current = next;
        }

        created
    }

    /// Set a value at path from a raw TOML string, with an optional inline comment.
    /// The value string should be a valid TOML value (e.g., "true", "\"max\"", "[]").
    pub fn set_raw_with_comment(&mut self, path: &str, toml_value: &str, comment: Option<&str>) -> RailResult<()> {
        // Parse the value as a standalone TOML assignment to extract the Value
        let parse_str = format!("__key__ = {}", toml_value);
        let parsed: DocumentMut = parse_str
            .parse()
            .map_err(|e| RailError::message(format!("Invalid TOML value '{}': {}", toml_value, e)))?;

        let mut value = parsed
            .get("__key__")
            .and_then(|i| i.as_value())
            .ok_or_else(|| RailError::message(format!("Failed to extract value from '{}'", toml_value)))?
            .clone();

        // Add inline comment if provided
        if let Some(c) = comment {
            value.decor_mut().set_suffix(format!("  # {}", c));
        }

        self.set(path, value)
    }

    /// Check if document has been modified from original content
    pub fn has_changes(&self) -> bool {
        self.doc.to_string() != self.original_content
    }

    /// Get the original content before modifications
    pub fn original_content(&self) -> &str {
        &self.original_content
    }

    /// Insert into array
    pub fn array_push(&mut self, path: &str, value: impl Into<Value>) -> RailResult<()> {
        let item = self
            .get_mut_item(path)
            .ok_or_else(|| RailError::message(format!("Path not found: {}", path)))?;

        if let Some(array) = item.as_array_mut() {
            array.push(value.into());
            Ok(())
        } else {
            Err(RailError::message(format!("Item at '{}' is not an array", path)))
        }
    }

    /// Helper to get mutable item
    fn get_mut_item(&mut self, path: &str) -> Option<&mut Item> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = self.doc.as_item_mut();

        for part in parts {
            let table = current.as_table_mut()?;
            current = table.get_mut(part)?;
        }

        Some(current)
    }

    /// Validate current state
    pub fn validate(&self) -> RailResult<()> {
        // Basic syntax check by printing and re-parsing
        let content = self.doc.to_string();
        content
            .parse::<DocumentMut>()
            .map(|_| ())
            .map_err(|e| RailError::message(format!("Validation failed: {}", e)))
    }

    /// Write changes (atomic via temp file)
    pub fn write(self) -> RailResult<()> {
        self.validate()?;
        crate::utils::write_file_atomic(&self.path, self.doc.to_string().as_bytes())
    }

    /// Write with backup (.bak file)
    pub fn write_with_backup(self) -> RailResult<()> {
        let backup_path = self.path.with_extension("toml.bak");
        fs::write(&backup_path, &self.original_content)
            .map_err(|e| RailError::message(format!("Failed to create backup: {}", e)))?;

        self.write()
    }

    /// Rollback changes (restore from backup)
    pub fn rollback(&self) -> RailResult<()> {
        let backup_path = self.path.with_extension("toml.bak");
        if backup_path.exists() {
            fs::rename(&backup_path, &self.path)
                .map_err(|e| RailError::message(format!("Failed to restore backup: {}", e)))?;
        }
        Ok(())
    }
}

/// Batch edit multiple TOML files atomically
#[derive(Debug, Default)]
pub struct TomlBatchEditor {
    editors: Vec<TomlEditor>,
}

impl TomlBatchEditor {
    /// Create a new batch editor
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an editor to the batch
    pub fn add(&mut self, editor: TomlEditor) -> &mut Self {
        self.editors.push(editor);
        self
    }

    /// Validate all editors
    pub fn validate_all(&self) -> RailResult<()> {
        for editor in &self.editors {
            editor.validate()?;
        }
        Ok(())
    }

    /// Write all (transactional - all or nothing)
    /// Note: True filesystem transactionality isn't possible, but we validate all before writing any.
    pub fn commit(self) -> RailResult<()> {
        self.validate_all()?;

        // Create backups for all
        for editor in &self.editors {
            let backup_path = editor.path.with_extension("toml.bak");
            fs::write(&backup_path, &editor.original_content).map_err(|e| {
                RailError::message(format!("Failed to create backup for {}: {}", editor.path.display(), e))
            })?;
        }

        // Write all
        for editor in self.editors {
            // If one fails, we should ideally rollback others, but `write` consumes editor.
            // In a real transactional system we'd need more complex logic.
            // For now, we rely on backups being present for manual recovery if needed.
            editor.write()?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_editor_set_value() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(file, "[package]\nname = \"old\"\n").unwrap();

        let mut editor = TomlEditor::open(file.path()).unwrap();
        editor.set("package.name", "new").unwrap();

        assert!(editor.doc.to_string().contains("name = \"new\""));
    }

    #[test]
    fn test_editor_set_preserves_unrelated_formatting() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        let original = r#"# package metadata
[package] # retained table comment
name = "old"
version = '0.1.0' # retained value comment

[package.metadata.'cargo-rail']
values = [
  "one", # retained array comment
  "two",
]
"#;
        write!(file, "{original}").unwrap();

        let mut editor = TomlEditor::open(file.path()).unwrap();
        assert!(!editor.has_changes());

        editor.set("package.name", "new").unwrap();

        assert_eq!(
            editor.doc.to_string(),
            original.replacen("name = \"old\"", "name = \"new\"", 1)
        );
    }

    #[test]
    fn test_editor_array_push() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(file, "[package]\nauthors = [\"me\"]\n").unwrap();

        let mut editor = TomlEditor::open(file.path()).unwrap();
        editor.array_push("package.authors", "you").unwrap();

        let content = editor.doc.to_string();
        assert!(content.contains("\"me\""));
        assert!(content.contains("\"you\""));
    }

    #[test]
    fn test_editor_hyphenated_section() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(file, "[package]\nname = \"test\"\n").unwrap();

        let mut editor = TomlEditor::open(file.path()).unwrap();

        // Ensure hyphenated section exists
        editor.ensure_section("change-detection");

        // Set a value in the hyphenated section
        editor
            .set_raw_with_comment(
                "change-detection.infrastructure",
                r#"[".github/**"]"#,
                Some("test comment"),
            )
            .unwrap();

        let content = editor.doc.to_string();
        assert!(
            content.contains("[change-detection]"),
            "Should have [change-detection] section, got:\n{}",
            content
        );
        assert!(
            content.contains("infrastructure"),
            "Should have infrastructure field, got:\n{}",
            content
        );
    }

    #[test]
    fn test_editor_contains_path_hyphenated() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(
            file,
            r#"[package]
name = "test"

[change-detection]
infrastructure = [".github/**"]
"#
        )
        .unwrap();

        let editor = TomlEditor::open(file.path()).unwrap();

        assert!(editor.contains_path("package.name"));
        assert!(editor.contains_path("change-detection.infrastructure"));
        assert!(!editor.contains_path("change-detection.custom"));
    }

    #[test]
    fn test_remove_prunes_empty_parent_tables() {
        let mut file = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(file, "[crates.demo.sync]").unwrap();

        let mut editor = TomlEditor::open(file.path()).unwrap();
        assert!(editor.remove("crates.demo.sync"));
        assert!(editor.doc().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_write_does_not_follow_a_fixed_name_temp_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("rail.toml");
        let victim = directory.path().join("victim");
        fs::write(&config, "value = \"old\"\n").unwrap();
        fs::write(&victim, "untouched\n").unwrap();
        symlink(&victim, config.with_extension("toml.tmp")).unwrap();

        let mut editor = TomlEditor::open(&config).unwrap();
        editor.set("value", "new").unwrap();
        editor.write().unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched\n");
        assert!(fs::read_to_string(&config).unwrap().contains("new"));
    }
}
