pub const fn portable_value() -> u8 {
    42
}

#[cfg(test)]
mod tests {
    #[test]
    fn returns_the_fixture_value() {
        assert_eq!(super::portable_value(), 42);
    }
}
