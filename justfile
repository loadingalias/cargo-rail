check:
    @scripts/check/check.sh

fix:
    @scripts/check/check.sh --fix

test crate="":
    @scripts/test/test.sh "{{ crate }}"

build:
    @cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface build --explain

build-release:
    @cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface build --explain -- --release

# Full Workspace Commands (no change detection)

check-all:
    @scripts/check/check.sh --all

test-all:
    @scripts/test/test.sh --all

build-all:
    cargo build --workspace --all-targets --all-features --locked

bench-unify packages="25" runs="10":
    @scripts/bench/unify.sh "{{ packages }}" "{{ runs }}"

gen-fixture members output:
    @scripts/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

# CI Commands (for GitHub Actions)

ci-check:
    @scripts/check/check.sh --all

# Explainability

plan:
    cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base -f json

dry-run surface="test":
    cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface {{ surface }} --dry-run --print-cmd --explain

# Maintenance

gen-docs:
    @scripts/docs/generate.sh

pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
