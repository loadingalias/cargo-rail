#[test]
fn portable_library_is_available_to_integration_targets() {
    assert_eq!(fixture_portable::portable_value(), 42);
}
