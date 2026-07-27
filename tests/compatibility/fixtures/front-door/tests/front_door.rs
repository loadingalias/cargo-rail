#[test]
fn links_the_library_into_an_integration_test() {
  assert_eq!(cargo_rail_compatibility_fixture::answer(), 42);
}
