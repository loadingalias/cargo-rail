check:
    @scripts/check/check.sh

test crate="":
    @scripts/test/test.sh "{{ crate }}"

build:
    @echo "Change Detection Plan:"
    @echo ""
    @cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base --explain
    @echo ""
    @echo "Building affected crates..."
    @cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface build

build-release:
    @echo "Change Detection Plan:"
    @echo ""
    @cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base --explain
    @echo ""
    @echo "Building affected crates (release)..."
    @cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface build -- --release

# Full Workspace Commands (no change detection)

check-all:
    @scripts/check/check.sh --all

test-all:
    @scripts/test/test.sh --all

build-all:
    cargo build --workspace --all-targets --all-features

build-release-all:
    cargo build --workspace --all-targets --all-features --release

# CI Commands (for GitHub Actions)

ci-check:
    @scripts/check/check.sh --ci

ci-test:
    @scripts/test/test.sh

ci-build:
    @cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --since "${RAIL_SINCE:-HEAD~1}" --surface build

# Explainability

plan:
    cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base -f json

why:
    cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base --explain

dry-run surface="test":
    cargo run --quiet --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --surface {{ surface }} --dry-run --print-cmd --explain

# Maintenance

update:
    cargo update --workspace
    cargo upgrade --recursive

gen-docs:
    @scripts/docs/generate.sh

pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
