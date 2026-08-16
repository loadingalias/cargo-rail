fn main() {
  println!("cargo::rerun-if-changed=native/value.c");
  println!("cargo::rerun-if-changed=native/plain.s");
  println!("cargo::rerun-if-changed=native/preprocessed.S");
  let target = std::env::var("TARGET").unwrap_or_default();
  let mut native = cc::Build::new();
  native.file("native/value.c");
  if !target.contains("msvc") {
    native.files(["native/plain.s", "native/preprocessed.S"]);
  }
  native.compile("fixture_native");
}
