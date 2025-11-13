/// Test crate B - no dependencies on other workspace crates
pub fn hello_from_b() -> String {
  "Hello from crate B!".to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_hello_from_b() {
    assert_eq!(hello_from_b(), "Hello from crate B!");
  }
}

pub fn new_feature() -> String {
  "This is a new feature!".to_string()
}

#[cfg(test)]
mod new_tests {
  use super::*;

  #[test]
  fn test_new_feature() {
    assert_eq!(new_feature(), "This is a new feature!");
  }
}

pub fn another_mono_feature() -> &'static str {
  "From monorepo"
}
// Change B
// Update to test-crate-b
pub fn feature_b1() -> &'static str {
  "Feature B1"
}
pub fn fix_b1() -> bool {
  true
}
pub fn sync_feature() -> &'static str {
  "Synced from monorepo"
}
