fn main() {
  assert_eq!(surface_conformance_core::used_by_product(), 7);
  assert_eq!(surface_conformance_core::feature_value(), 11);
}

pub fn live_binary_surface() {}

pub fn dead_binary_surface() {}

#[cfg(test)]
mod tests {
  #[test]
  fn reaches_test_support() {
    super::test_support();
  }
}

#[cfg(test)]
pub fn test_support() {}
