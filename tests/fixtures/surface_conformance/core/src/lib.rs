//! Focused cross-analyzer surface corpus.

/// Used by the shipped application.
///
/// ```
/// assert_eq!(surface_conformance_core::used_by_product(), 7);
/// ```
///
/// ```compile_fail,E0308
/// let _: usize = "this doctest must fail to compile";
/// ```
pub fn used_by_product() -> usize {
  internal::value()
}

#[cfg(feature = "extra")]
pub fn feature_value() -> usize {
  11
}

pub fn dead_public() {}

mod internal {
  pub(super) fn value() -> usize {
    7
  }

  pub(crate) fn dead_crate_visible() {}
}

pub struct Record {
  pub first: usize,
  pub second: usize,
}

pub enum Mode {
  Used,
  Dead,
}
