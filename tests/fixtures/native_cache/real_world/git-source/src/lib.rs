pub fn git_value() -> u64 {
  if cfg!(feature = "extended") { 17 } else { 11 }
}
