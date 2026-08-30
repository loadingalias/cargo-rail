pub fn observed_portable_value() -> u8 {
    fixture_portable::portable_value()
}

#[cfg(test)]
mod tests {
    #[test]
    fn observes_the_portable_fixture() {
        assert_eq!(super::observed_portable_value(), 42);
    }
}
