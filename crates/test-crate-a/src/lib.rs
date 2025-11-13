/// Test crate A - depends on test-crate-b
use test_crate_b::hello_from_b;

pub fn hello() -> String {
  format!("Hello from crate A! {}", hello_from_b())
}
// New comment from monorepo test
// Mono change 2
// Change A
// Change from monorepo
pub fn new_monorepo_function() {
  println!("Added from monorepo");
}

// Change from remote repo
pub fn from_remote() {
  println!("This change came from the split repo");
}
pub fn feature_a1() -> &'static str {
  "Feature A1 - MONOREPO VERSION"
}
pub fn fix_a1() -> bool {
  false
}
// Update to test-crate-a

// Phase 3 Test 3.2: New feature added in monorepo
pub fn phase3_feature() -> &'static str {
  "This is a new feature from Phase 3 testing"
}

// Phase 3 Test 3.3: New feature added in split repo
pub fn split_repo_feature() -> &'static str {
  "This change originates from the split repo"
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_hello() {
    let msg = hello();
    assert!(msg.contains("Hello from crate A"));
    assert!(msg.contains("Hello from crate B"));
  }
}
