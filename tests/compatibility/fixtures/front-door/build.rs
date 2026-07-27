fn main() {
  const RESPONSE_FILE: &str = "CARGO_RAIL_COMPAT_LINK_RESPONSE";
  println!("cargo:rerun-if-env-changed={RESPONSE_FILE}");
  if let Ok(path) = std::env::var(RESPONSE_FILE) {
    println!("cargo:rustc-link-arg=@{path}");
  }
}
