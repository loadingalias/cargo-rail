use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
  for name in [
    "CARGO_RAIL_COMPILER_CACHE_WRAPPER",
    "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY",
    "CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT",
    "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION",
  ] {
    assert!(env::var_os(name).is_none(), "private compiler-cache environment leaked into build.rs: {name}");
  }
  println!("cargo::rerun-if-env-changed=P74_GENERATED_VALUE");
  let value = env::var("P74_GENERATED_VALUE").unwrap_or_else(|_| "23".to_string());
  let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("generated.rs");
  fs::write(output, format!("pub const GENERATED_VALUE: u64 = {value};\n")).expect("write generated fixture");
}
