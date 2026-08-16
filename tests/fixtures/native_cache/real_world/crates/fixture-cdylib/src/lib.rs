use fixture_types::Record;

#[unsafe(no_mangle)]
pub extern "C" fn fixture_cdylib_value() -> u64 {
  Record::new(43, "C dynamic library").id
}
