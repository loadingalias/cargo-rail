fn main() {
  #[cfg(feature = "variant")]
  println!("cargo-rail WASI compatibility variant");
  #[cfg(not(feature = "variant"))]
  println!("{}", cargo_rail_compat_wasi::MESSAGE);
}
