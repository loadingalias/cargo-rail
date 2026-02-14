build:
    cargo build --workspace --all-targets --all-features
    @echo "✅ Success!"

plan:
    cargo rail plan --merge-base -f json

run:
    cargo rail run --merge-base --profile local

ci:
    @if [ -z "${RAIL_SINCE:-}" ]; then \
        echo "RAIL_SINCE is required (example: export RAIL_SINCE=origin/main)"; \
        exit 2; \
    fi
    cargo rail run --since "$RAIL_SINCE" --profile ci

explain:
    cargo rail plan --merge-base --explain

build-release:
    cargo build --workspace --all-targets --all-features --release
    @echo "✅ Success!"

test crate="":
    @scripts/test/test.sh "{{ crate }}"

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

update:
    cargo update --workspace
    cargo upgrade --recursive

gen-docs:
    @scripts/docs/generate.sh

# Pin GitHub Actions to commit SHAs for security
pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

# Verify all GitHub Actions are properly pinned
verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
