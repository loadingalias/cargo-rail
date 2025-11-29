//! Safe TOML file mutation operations

use crate::error::{RailError, RailResult};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Safe TOML file editor with validation and atomic writes
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
      if let Some(table) = current.as_table() {
        current = table.get(part)?;
      } else {
        // We can't easily return &Item from InlineTable because it returns &Value
        // and we can't construct a temporary &Item.
        // For now, we only support navigating standard tables for `get`.
        // If we hit an inline table, we stop.
        return None;
      }
    }

    Some(current)
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
      if let Some(table) = current.as_table_mut() {
        current = table.get_mut(part)?;
      } else {
        return None;
      }
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

    let content = self.doc.to_string();
    let temp_path = self.path.with_extension("toml.tmp");

    fs::write(&temp_path, content).map_err(|e| RailError::message(format!("Failed to write temp file: {}", e)))?;

    fs::rename(&temp_path, &self.path).map_err(|e| RailError::message(format!("Failed to rename temp file: {}", e)))?;

    Ok(())
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
#[derive(Default)]
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
      fs::write(&backup_path, &editor.original_content)
        .map_err(|e| RailError::message(format!("Failed to create backup for {}: {}", editor.path.display(), e)))?;
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
}
