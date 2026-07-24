unsafe extern "C" {
  fn fixture_native_value() -> u64;
}

pub fn native_value() -> u64 {
  // SAFETY: the crate's build script compiles this exact no-argument function.
  unsafe { fixture_native_value() }
}
