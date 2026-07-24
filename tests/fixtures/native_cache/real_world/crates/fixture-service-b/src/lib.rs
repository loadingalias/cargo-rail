use regex::Regex;

pub fn service_value() -> u64 {
  let route = Regex::new("^fixture-[a-z]+$").expect("fixture route is valid");
  fixture_api::response().id
    + fixture_native::native_value()
    + u64::from(route.is_match("fixture-service"))
    + parallel_checksum()
}

#[cfg(feature = "parallel")]
fn parallel_checksum() -> u64 {
  use rayon::prelude::*;

  (1..=3).into_par_iter().sum()
}

#[cfg(not(feature = "parallel"))]
fn parallel_checksum() -> u64 {
  0
}
