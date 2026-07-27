# Remote development. Provider mechanics and credentials stay in ~/dev-machines.

ssh-list:
    @"$HOME/dev-machines/dev-machine" list

ssh target:
    @"$HOME/dev-machines/dev-machine" ssh auto "{{ target }}"

ssh-check target:
    @"$HOME/dev-machines/dev-machine" ssh auto "{{ target }}" --check

ssh-create target *args="":
    @"$HOME/dev-machines/dev-machine" create auto "{{ target }}" {{ args }}

ssh-start target:
    @"$HOME/dev-machines/dev-machine" start auto "{{ target }}"

ssh-deallocate target:
    @"$HOME/dev-machines/dev-machine" deallocate auto "{{ target }}"

ssh-kill target:
    @"$HOME/dev-machines/dev-machine" kill auto "{{ target }}"

ssh-status target="":
    @if [ -n "{{ target }}" ]; then "$HOME/dev-machines/dev-machine" status auto "{{ target }}"; else "$HOME/dev-machines/dev-machine" status auto; fi

ssh-bootstrap target:
    @"$HOME/dev-machines/dev-machine" bootstrap auto "{{ target }}"

check:
    @scripts/check/check.sh

fix:
    @scripts/check/check.sh --fix

test crate="":
    @scripts/test/test.sh "{{ crate }}"

build:
    @cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --action build --explain

build-release:
    cargo build --workspace --all-targets --all-features --release --locked

# Full Workspace Commands (no change detection)

test-all:
    @scripts/test/test.sh --all

build-all:
    cargo build --workspace --all-targets --all-features --locked

bench-unify packages="25" runs="10":
    @scripts/bench/unify.sh "{{ packages }}" "{{ runs }}"

bench-native-cache runs="10":
    @scripts/bench/native-cache.sh "{{ runs }}"

gen-fixture members output:
    @scripts/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

# CI Commands (for GitHub Actions)

ci-check:
    @scripts/check/check.sh

# Explainability

plan:
    cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base -f json

dry-run action="test":
    cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail run --merge-base --action {{ action }} --dry-run --print-cmd --explain

# Maintenance

gen-docs:
    @scripts/docs/generate.sh

pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
