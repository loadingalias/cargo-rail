build:
    cargo build --workspace --all-targets --all-features
    @echo "✅ Success!"

test:
    cargo nextest run --workspace --all-targets --all-features --config-file .config/nextest.toml
    @echo "✅ Tests passed!"

check:
    cargo fmt --all
    cargo check --workspace --all-targets --all-features
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty -- -D warnings
    cargo deny check all
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
    cargo audit
    @echo "✅ All checks passed!"

ci-check:
    cargo fmt --all -- --check
    cargo check --workspace --all-targets --all-features
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo deny check all
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
    cargo audit
    @echo "✅ CI checks passed!"

waste:
    cargo +nightly udeps --workspace --all-targets --all-features

update:
    cargo update --workspace
    cargo upgrade --recursive