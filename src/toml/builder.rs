//! Deterministic Cargo manifest section builders.

use super::format::{TomlFormatter, TomlValue};
use crate::error::RailResult;

/// Builder for a deterministic `[workspace.dependencies]` section.
#[derive(Debug)]
pub struct WorkspaceDepsBuilder {
    formatter: TomlFormatter,
    deps: Vec<(String, String, Option<String>)>,
}

impl Default for WorkspaceDepsBuilder {
    fn default() -> Self {
        Self {
            formatter: TomlFormatter::new(),
            deps: Vec::new(),
        }
    }
}

impl WorkspaceDepsBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a dependency value.
    pub fn add(&mut self, name: &str, value: &str) -> &mut Self {
        self.deps.push((name.to_string(), value.to_string(), None));
        self
    }

    /// Add a dependency value with an inline comment.
    pub fn add_with_comment(&mut self, name: &str, value: &str, comment: &str) -> &mut Self {
        self.deps
            .push((name.to_string(), value.to_string(), Some(comment.to_string())));
        self
    }

    /// Add a dependency represented by an inline table.
    pub fn add_table(&mut self, name: &str, pairs: &[(String, TomlValue)]) -> &mut Self {
        let value = self.formatter.inline_table(pairs);
        self.deps.push((name.to_string(), value, None));
        self
    }

    /// Add an inline-table dependency with an inline comment.
    pub fn add_table_with_comment(&mut self, name: &str, pairs: &[(String, TomlValue)], comment: &str) -> &mut Self {
        let value = self.formatter.inline_table(pairs);
        self.deps.push((name.to_string(), value, Some(comment.to_string())));
        self
    }

    /// Build the `[workspace.dependencies]` section in lexical dependency order.
    pub fn build(&self) -> RailResult<String> {
        let mut content = String::from("\n[workspace.dependencies]\n");
        let mut indices: Vec<usize> = (0..self.deps.len()).collect();
        indices.sort_by(|&left, &right| self.deps[left].0.cmp(&self.deps[right].0));

        for index in indices {
            let (name, value, comment) = &self.deps[index];
            if let Some(comment) = comment {
                content.push_str(&format!("{name} = {value}  # {comment}\n"));
            } else {
                content.push_str(&format!("{name} = {value}\n"));
            }
        }

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_dependencies_are_always_sorted() {
        let mut builder = WorkspaceDepsBuilder::new();
        builder.add("zeta", "\"1\"").add("alpha", "\"2\"");

        let output = builder.build().expect("valid workspace dependency table");
        assert!(output.find("alpha").unwrap() < output.find("zeta").unwrap());
    }
}
