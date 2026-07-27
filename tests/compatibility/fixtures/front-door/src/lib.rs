#![forbid(unsafe_code)]

#[cfg(feature = "message")]
pub const MESSAGE: &str = "cargo-rail compatibility";

#[cfg(not(feature = "message"))]
pub const MESSAGE: &str = "cargo-rail compatibility without defaults";

pub fn answer() -> u8 {
  42
}

#[cfg(test)]
mod tests {
  #[test]
  fn exposes_the_fixture_contract() {
    assert_eq!(super::answer(), 42);
    assert!(super::MESSAGE.starts_with("cargo-rail"));
  }
}
