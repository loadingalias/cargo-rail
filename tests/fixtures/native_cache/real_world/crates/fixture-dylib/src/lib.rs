use fixture_types::Record;

pub fn fixture_dylib_value() -> u64 {
  Record::new(41, "Rust dynamic library").id
}
