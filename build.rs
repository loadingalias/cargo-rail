fn main() {
  if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
    // Clap's debug command builder needs more headroom than MSVC's 1 MiB default.
    // PE reserves virtual address space here; physical pages remain demand-committed.
    println!("cargo::rustc-link-arg-bin=cargo-rail=/STACK:4194304");
  }
}
