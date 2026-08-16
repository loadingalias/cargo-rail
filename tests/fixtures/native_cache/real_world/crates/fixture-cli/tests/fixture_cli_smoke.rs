#[test]
fn workspace_services_compose() {
  assert_eq!(fixture_service_a::service_value() + fixture_service_b::service_value(), 119);
}
