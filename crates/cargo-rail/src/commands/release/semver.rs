//! Semver analysis and breaking change detection

use crate::core::error::RailResult;
use serde::{Deserialize, Serialize};

/// Version bump type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BumpType {
    Major,
    Minor,
    Patch,
    None,
}

impl BumpType {
    /// Apply bump to a semver version string
    pub fn apply(&self, current: &str) -> RailResult<String> {
        let mut version: semver::Version = current
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid semver version '{}': {}", current, e))?;

        match self {
            BumpType::Major => {
                version.major += 1;
                version.minor = 0;
                version.patch = 0;
            }
            BumpType::Minor => {
                version.minor += 1;
                version.patch = 0;
            }
            BumpType::Patch => {
                version.patch += 1;
            }
            BumpType::None => {}
        }

        Ok(version.to_string())
    }

    /// Combine two bump types (returns the larger bump)
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (BumpType::Major, _) | (_, BumpType::Major) => BumpType::Major,
            (BumpType::Minor, _) | (_, BumpType::Minor) => BumpType::Minor,
            (BumpType::Patch, _) | (_, BumpType::Patch) => BumpType::Patch,
            _ => BumpType::None,
        }
    }
}

/// Analyze semver compatibility between versions
pub fn analyze_semver_changes(
    _crate_name: &str,
    _current_version: &str,
) -> RailResult<BumpType> {
    // TODO: Integrate cargo-semver-checks
    Ok(BumpType::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bump_major() {
        assert_eq!(BumpType::Major.apply("1.2.3").unwrap(), "2.0.0");
        assert_eq!(BumpType::Major.apply("0.5.1").unwrap(), "1.0.0");
    }

    #[test]
    fn test_bump_minor() {
        assert_eq!(BumpType::Minor.apply("1.2.3").unwrap(), "1.3.0");
        assert_eq!(BumpType::Minor.apply("0.1.5").unwrap(), "0.2.0");
    }

    #[test]
    fn test_bump_patch() {
        assert_eq!(BumpType::Patch.apply("1.2.3").unwrap(), "1.2.4");
        assert_eq!(BumpType::Patch.apply("0.1.0").unwrap(), "0.1.1");
    }

    #[test]
    fn test_bump_none() {
        assert_eq!(BumpType::None.apply("1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn test_combine_bumps() {
        assert_eq!(BumpType::Major.combine(BumpType::Minor), BumpType::Major);
        assert_eq!(BumpType::Minor.combine(BumpType::Patch), BumpType::Minor);
        assert_eq!(BumpType::Patch.combine(BumpType::None), BumpType::Patch);
        assert_eq!(BumpType::None.combine(BumpType::None), BumpType::None);
    }
}
