/// Test crate A - depends on test-crate-b
use test_crate_b::hello_from_b;

pub fn hello() -> String {
  format!("Hello from crate A! {}", hello_from_b())
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
