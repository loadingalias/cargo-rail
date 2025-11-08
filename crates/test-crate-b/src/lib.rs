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
