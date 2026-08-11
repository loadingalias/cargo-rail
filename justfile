[windows]
set shell := ["bash", "-lc"]

# Remote development. Provider mechanics and credentials stay in ~/dev-machines.

ssh-list:
    @"$HOME/dev-machines/dev-machine" list

ssh target:
    @"$HOME/dev-machines/dev-machine" ssh cargo-rail "{{ target }}"

ssh-check target:
    @"$HOME/dev-machines/dev-machine" ssh cargo-rail "{{ target }}" --check

ssh-create target *args="":
    @"$HOME/dev-machines/dev-machine" create cargo-rail "{{ target }}" {{ args }}

ssh-kill target:
    @"$HOME/dev-machines/dev-machine" kill cargo-rail "{{ target }}"

ssh-status target:
    @"$HOME/dev-machines/dev-machine" status cargo-rail "{{ target }}"

ssh-bootstrap target:
    @"$HOME/dev-machines/dev-machine" bootstrap cargo-rail "{{ target }}"

ssh-qualification-tools target:
    @"$HOME/dev-machines/dev-machine" just cargo-rail "{{ target }}" "install-qualification-tools"

ssh-just target recipe *args="":
    @"$HOME/dev-machines/dev-machine" just cargo-rail "{{ target }}" "{{ recipe }}" {{ args }}

ssh-collect-bench target run_id destination:
    @"$HOME/dev-machines/dev-machine" collect-bench cargo-rail "{{ target }}" "{{ run_id }}" "{{ destination }}"

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

bench-native-cache runs:
    @scripts/bench/native-cache.sh run "{{ runs }}"

bench-native-cache-smoke:
    @scripts/bench/native-cache.sh smoke

bench-native-cache-resume results:
    @scripts/bench/native-cache.sh resume "{{ results }}"

bench-native-cache-summarize results:
    @scripts/bench/native-cache-report.sh summarize "{{ results }}"

bench-native-cache-validate results:
    @scripts/bench/native-cache-report.sh validate "{{ results }}"

bench-native-cache-aws-plan target:
    @scripts/bench/remote-native-cache.sh plan "{{ target }}"

bench-native-cache-aws-smoke target *args="":
    @scripts/bench/remote-native-cache.sh smoke "{{ target }}" {{ args }}

bench-native-cache-aws target runs execute:
    @scripts/bench/remote-native-cache.sh run "{{ target }}" "{{ runs }}" "{{ execute }}"

bench-native-cache-remote mode runs run_id:
    @scripts/bench/native-cache-remote-dispatch.sh "{{ mode }}" "{{ runs }}" "{{ run_id }}"

install-qualification-tools:
    @scripts/ci/install-qualification-tools.sh

bench-native-cache-archive run_id:
    @scripts/bench/native-cache-archive.sh "{{ run_id }}"

gen-fixture members output:
    @scripts/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

# CI Commands (for GitHub Actions)

check-ci:
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
