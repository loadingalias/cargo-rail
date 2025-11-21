//! Syntactic version requirement merging
//!
//! Fallback strategy when Cargo's resolution isn't available.
//! Automatically merges compatible version requirements to eliminate false conflicts.
//! Only used when resolution-based checking can't determine compatibility.

use semver::VersionReq;

/// Try to merge two compatible version requirements syntactically
///
/// This is a FALLBACK when resolution-based checking isn't available.
/// Implements conservative merging:
/// - Identical requirements → return as-is
/// - Compatible carets → merge to most restrictive
/// - Otherwise → None (incompatible)
///
/// # Examples
/// ```rust,ignore
/// use semver::VersionReq;
/// use cargo_rail::cargo::unify::version_merge::try_merge_version_reqs;
///
/// let v1 = VersionReq::parse("^1.2.0").unwrap();
/// let v2 = VersionReq::parse("^1.3.0").unwrap();
/// assert_eq!(try_merge_version_reqs(&v1, &v2), Some(v2));
/// ```
pub fn try_merge_version_reqs(v1: &VersionReq, v2: &VersionReq) -> Option<VersionReq> {
  // Fast path: identical requirements
  if v1 == v2 {
    return Some(v1.clone());
  }

  // Parse both as simple caret requirements
  let comp1 = v1.comparators.as_slice();
  let comp2 = v2.comparators.as_slice();

  // Only merge if both have exactly one comparator
  if comp1.len() != 1 || comp2.len() != 1 {
    return None;
  }

  let c1 = &comp1[0];
  let c2 = &comp2[0];

  // Must both be Caret operators
  if c1.op != semver::Op::Caret || c2.op != semver::Op::Caret {
    return None;
  }

  // Extract major.minor.patch
  let (maj1, min1, patch1) = (c1.major, c1.minor?, c1.patch?);
  let (maj2, min2, patch2) = (c2.major, c2.minor?, c2.patch?);

  // Different major versions cannot be merged
  if maj1 != maj2 {
    return None;
  }

  // For 0.x versions, minor must match (0.x.y is NOT compatible with 0.z.w)
  if maj1 == 0 {
    if min1 != min2 {
      return None;
    }
    // Same 0.x - pick higher patch
    let higher_patch = patch1.max(patch2);
    return Some(VersionReq {
      comparators: vec![semver::Comparator {
        op: semver::Op::Caret,
        major: 0,
        minor: Some(min1),
        patch: Some(higher_patch),
        pre: semver::Prerelease::EMPTY,
      }],
    });
  }

  // For 1.x+ versions, different minors CAN be merged (^1.2 and ^1.3 → ^1.3)
  // Pick the higher minor.patch combination
  let use_second = if min1 == min2 { patch2 > patch1 } else { min2 > min1 };

  let (maj, min, patch) = if use_second {
    (maj2, min2, patch2)
  } else {
    (maj1, min1, patch1)
  };

  Some(VersionReq {
    comparators: vec![semver::Comparator {
      op: semver::Op::Caret,
      major: maj,
      minor: Some(min),
      patch: Some(patch),
      pre: semver::Prerelease::EMPTY,
    }],
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_merge_identical() {
    let v1 = VersionReq::parse("^1.0.0").unwrap();
    let v2 = VersionReq::parse("^1.0.0").unwrap();
    assert_eq!(try_merge_version_reqs(&v1, &v2), Some(v1));
  }

  #[test]
  fn test_merge_compatible_carets() {
    let v1 = VersionReq::parse("^1.2.0").unwrap();
    let v2 = VersionReq::parse("^1.3.0").unwrap();
    let merged = try_merge_version_reqs(&v1, &v2).unwrap();
    assert_eq!(merged.to_string(), "^1.3.0");
  }

  #[test]
  fn test_merge_different_major() {
    let v1 = VersionReq::parse("^1.0.0").unwrap();
    let v2 = VersionReq::parse("^2.0.0").unwrap();
    assert!(try_merge_version_reqs(&v1, &v2).is_none());
  }

  #[test]
  fn test_merge_0x_requires_matching_minor() {
    let v1 = VersionReq::parse("^0.2.0").unwrap();
    let v2 = VersionReq::parse("^0.3.0").unwrap();
    assert!(try_merge_version_reqs(&v1, &v2).is_none());
  }

  #[test]
  fn test_merge_0x_same_minor() {
    let v1 = VersionReq::parse("^0.2.1").unwrap();
    let v2 = VersionReq::parse("^0.2.3").unwrap();
    let merged = try_merge_version_reqs(&v1, &v2).unwrap();
    assert_eq!(merged.to_string(), "^0.2.3");
  }
}
