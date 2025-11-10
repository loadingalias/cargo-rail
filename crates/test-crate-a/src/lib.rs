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
// Update to test-crate-a

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
