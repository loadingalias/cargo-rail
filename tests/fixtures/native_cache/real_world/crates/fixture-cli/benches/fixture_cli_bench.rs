#[test]
fn benchmark_target_compiles_against_the_workspace() {
  assert_eq!(fixture_service_a::service_value() + fixture_service_b::service_value(), 119);
}
