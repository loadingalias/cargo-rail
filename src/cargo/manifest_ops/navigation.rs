//! TOML section navigation and target-specific dependencies

use crate::error::{RailError, RailResult};
use toml_edit::{DocumentMut, Item, Table};

// TOML Section Navigation

/// Ensure a section exists, creating if necessary
pub fn ensure_section(doc: &mut DocumentMut, path: &str) -> RailResult<()> {
    let parts = path.split('.');
    if parts.clone().any(str::is_empty) {
        return Err(RailError::message("Empty path provided"));
    }

    let mut current = doc.as_item_mut();

    for part in parts {
        if let Some(table) = current.as_table_mut() {
            if !table.contains_key(part) {
                table.insert(part, Item::Table(Table::new()));
            }
            current = table
                .get_mut(part)
                .ok_or_else(|| RailError::message(format!("Failed to access {}", part)))?;
        } else {
            return Err(RailError::message(
                "cannot create section: expected table but found non-table",
            ));
        }
    }

    Ok(())
}

/// Get or create a table at dotted path
pub fn get_or_create_table<'a>(doc: &'a mut DocumentMut, path: &str) -> RailResult<&'a mut Table> {
    let parts = path.split('.');
    if parts.clone().any(str::is_empty) {
        return Err(RailError::message("Empty path provided"));
    }

    let mut current = doc.as_item_mut();

    for part in parts {
        if let Some(table) = current.as_table_mut() {
            if !table.contains_key(part) {
                table.insert(part, Item::Table(Table::new()));
            }
            current = table
                .get_mut(part)
                .ok_or_else(|| RailError::message(format!("Failed to access {}", part)))?;
        } else {
            return Err(RailError::message(format!(
                "cannot navigate to {}: parent is not a table",
                part
            )));
        }
    }

    current
        .as_table_mut()
        .ok_or_else(|| RailError::message(format!("Final item at path '{}' is not a table", path)))
}

/// Insert dependency into a section
pub fn insert_dependency(section: &mut Table, name: &str, entry: Item) -> RailResult<()> {
    section.insert(name, entry);
    Ok(())
}

// Target-Specific Dependencies

/// Add target-specific dependency
pub fn insert_target_dependency(
    doc: &mut DocumentMut,
    target: &str,
    section: &str,
    name: &str,
    entry: Item,
) -> RailResult<()> {
    let target_section = get_target_section_mut(doc, target, section)?;
    target_section.insert(name, entry);
    Ok(())
}

/// Remove a dependency from target-specific section
pub fn remove_target_dependency(doc: &mut DocumentMut, target: &str, section: &str, name: &str) -> RailResult<()> {
    if let Some(target_section) = doc
        .get_mut("target")
        .and_then(|item| item.get_mut(target))
        .and_then(|item| item.get_mut(section))
        .and_then(Item::as_table_mut)
    {
        target_section.remove(name);
    }
    Ok(())
}

/// Get target-specific dependencies section (mutable)
fn get_target_section_mut<'a>(doc: &'a mut DocumentMut, target: &str, section: &str) -> RailResult<&'a mut Table> {
    let mut table = doc.as_table_mut();
    for key in ["target", target, section] {
        table = table
            .entry(key)
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| RailError::message(format!("cannot navigate to {}: expected table", key)))?;
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::{DocumentMut, Item, Value};

    #[test]
    fn test_ensure_section_creates_new() {
        let content = "[package]\nname = \"test\"\n";
        let mut doc: DocumentMut = content.parse().unwrap();

        ensure_section(&mut doc, "workspace").unwrap();
        assert!(doc.contains_key("workspace"));
    }

    #[test]
    fn test_ensure_section_nested() {
        let content = "";
        let mut doc: DocumentMut = content.parse().unwrap();

        ensure_section(&mut doc, "workspace.dependencies").unwrap();
        assert!(doc.contains_key("workspace"));
        assert!(doc["workspace"].as_table().unwrap().contains_key("dependencies"));
    }

    #[test]
    fn test_get_or_create_table() {
        let content = "";
        let mut doc: DocumentMut = content.parse().unwrap();

        let table = get_or_create_table(&mut doc, "workspace.dependencies").unwrap();
        assert!(table.is_empty());

        // Should work again without error
        let table2 = get_or_create_table(&mut doc, "workspace.dependencies").unwrap();
        assert!(table2.is_empty());
    }

    #[test]
    fn test_insert_dependency() {
        let content = "[dependencies]\n";
        let mut doc: DocumentMut = content.parse().unwrap();

        let deps = get_or_create_table(&mut doc, "dependencies").unwrap();
        let entry = Item::Value(Value::from("1.0"));
        insert_dependency(deps, "serde", entry).unwrap();

        assert!(deps.contains_key("serde"));
    }

    #[test]
    fn target_dependency_keys_are_literal_and_removal_is_scoped() {
        for target in [
            "cfg(unix)",
            "thumbv8m.main-none-eabi",
            "cfg(target_feature = \"sse4.2\")",
        ] {
            let mut doc = DocumentMut::new();
            insert_target_dependency(
                &mut doc,
                target,
                "dependencies",
                "libc",
                Item::Value(Value::from("1.0")),
            )
            .unwrap();
            insert_target_dependency(
                &mut doc,
                target,
                "dependencies",
                "keep",
                Item::Value(Value::from("2.0")),
            )
            .unwrap();
            let parsed: DocumentMut = doc.to_string().parse().unwrap();
            assert_eq!(parsed["target"].as_table().unwrap().len(), 1);
            assert_eq!(parsed["target"][target]["dependencies"]["libc"].as_str(), Some("1.0"));
            remove_target_dependency(&mut doc, target, "dependencies", "libc").unwrap();
            assert!(
                !doc["target"][target]["dependencies"]
                    .as_table()
                    .unwrap()
                    .contains_key("libc")
            );
            assert_eq!(doc["target"][target]["dependencies"]["keep"].as_str(), Some("2.0"));
        }
    }

    #[test]
    fn empty_path_components_are_rejected_without_mutation() {
        for path in ["", ".workspace", "workspace.", "workspace..dependencies"] {
            let mut doc = DocumentMut::new();
            assert!(ensure_section(&mut doc, path).is_err());
            get_or_create_table(&mut doc, path).unwrap_err();
            assert_eq!(doc.to_string(), "");
        }
    }
}
