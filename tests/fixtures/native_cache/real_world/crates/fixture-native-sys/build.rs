fn main() {
  println!("cargo::rerun-if-changed=native/value.c");
  cc::Build::new().file("native/value.c").compile("fixture_native");
}
