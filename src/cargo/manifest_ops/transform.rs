//! Batch transformation operations

use crate::cargo::manifest_analyzer::DepKind;
use crate::error::{RailError, RailResult};
use toml_edit::{DocumentMut, Item, Table, Value};

use super::workspace_ref::is_package_workspace_inherited;

/// Resolve workspace references in package fields
pub fn resolve_package_workspace_inheritance(doc: &mut DocumentMut, workspace_package: &Table) -> RailResult<()> {
    let fields = [
        "version",
        "authors",
        "edition",
        "rust-version",
        "license",
        "repository",
        "description",
        "homepage",
        "documentation",
        "readme",
        "keywords",
        "categories",
    ];

    if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_mut()) {
        for field in fields {
            if let Some(item) = package.get(field)
                && is_package_workspace_inherited(item)
            {
                // Get value from workspace.package
                if let Some(workspace_value) = workspace_package.get(field) {
                    package.insert(field, workspace_value.clone());
                } else {
                    package.remove(field);
                }
            }
        }

        // Remove workspace reference from package itself
        package.remove("workspace");
    }

    Ok(())
}

/// Set version on a dependency entry
pub fn set_version(item: &mut Item, version: &str) -> RailResult<()> {
    // If it's a simple string, replace the entire item
    if item.as_str().is_some() {
        *item = Item::Value(Value::from(version));
        return Ok(());
    }

    // If it's a table, update the version field
    if let Some(table) = item.as_inline_table_mut() {
        table.insert("version", Value::from(version));
        Ok(())
    } else if let Some(table) = item.as_table_mut() {
        table.insert("version", Item::Value(Value::from(version)));
        Ok(())
    } else {
        Err(RailError::message(
            "cannot set version: item is not a valid dependency format",
        ))
    }
}

/// Convert DepKind to section name string
pub fn dep_kind_to_section(kind: DepKind) -> &'static str {
    match kind {
        DepKind::Normal => "dependencies",
        DepKind::Dev => "dev-dependencies",
        DepKind::Build => "build-dependencies",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

    #[test]
    fn test_resolve_package_workspace_inheritance() {
        let content = "[package]\nname = \"test\"\nversion = { workspace = true }\n";
        let mut doc: DocumentMut = content.parse().unwrap();

        let mut workspace_package = Table::new();
        workspace_package.insert("version", Item::Value(Value::from("1.2.3")));

        resolve_package_workspace_inheritance(&mut doc, &workspace_package).unwrap();

        let package = doc.get("package").unwrap().as_table().unwrap();
        let version = package.get("version").unwrap();
        assert_eq!(version.as_value().unwrap().as_str().unwrap(), "1.2.3");
    }

    #[test]
    fn test_set_version_all_formats() {
        // Simple string
        let mut item1 = Item::Value(Value::from("1.0"));
        set_version(&mut item1, "2.0").unwrap();
        assert_eq!(item1.as_str().unwrap(), "2.0");

        // Inline table
        let mut table = InlineTable::new();
        table.insert("version", Value::from("1.0"));
        let mut item2 = Item::Value(Value::InlineTable(table));
        set_version(&mut item2, "3.0").unwrap();
        assert_eq!(
            item2
                .as_inline_table()
                .unwrap()
                .get("version")
                .unwrap()
                .as_str()
                .unwrap(),
            "3.0"
        );
    }

    #[test]
    fn test_dep_kind_to_section() {
        assert_eq!(dep_kind_to_section(DepKind::Normal), "dependencies");
        assert_eq!(dep_kind_to_section(DepKind::Dev), "dev-dependencies");
        assert_eq!(dep_kind_to_section(DepKind::Build), "build-dependencies");
    }
}
