#[test]
fn benchmark_target_compiles_against_the_library() {
    assert_eq!(fixture_portable::portable_value(), 42);
}
