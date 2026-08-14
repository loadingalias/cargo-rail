[windows]
set shell := ["bash", "-lc"]

# Remote development. Provider mechanics and credentials stay in ~/dev-machines.

dev_machine := env_var_or_default("DEV_MACHINE_BIN", env_var("HOME") + "/dev-machines/dev-machine")

ssh-list:
    @"{{ dev_machine }}" list

ssh target *args="":
    @"{{ dev_machine }}" ssh cargo-rail "{{ target }}" {{ args }}

ssh-check target *args="":
    @"{{ dev_machine }}" ssh cargo-rail "{{ target }}" --check {{ args }}

ssh-preflight target:
    @"{{ dev_machine }}" preflight cargo-rail "{{ target }}"

ssh-create target *args="":
    @"{{ dev_machine }}" create cargo-rail "{{ target }}" {{ args }}

ssh-start target:
    @"{{ dev_machine }}" start cargo-rail "{{ target }}"

ssh-deallocate target:
    @"{{ dev_machine }}" deallocate cargo-rail "{{ target }}"

ssh-kill target:
    @"{{ dev_machine }}" kill cargo-rail "{{ target }}"

ssh-status target="":
    @if [ -n "{{ target }}" ]; then "{{ dev_machine }}" status cargo-rail "{{ target }}"; else "{{ dev_machine }}" status cargo-rail; fi

ssh-bootstrap target profile="":
    @if [ -n "{{ profile }}" ]; then "{{ dev_machine }}" bootstrap cargo-rail "{{ target }}" "{{ profile }}"; else "{{ dev_machine }}" bootstrap cargo-rail "{{ target }}"; fi

ssh-qualification-tools target:
    @"{{ dev_machine }}" just cargo-rail "{{ target }}" "install-qualification-tools"

ssh-just target *args="":
    @"{{ dev_machine }}" just cargo-rail "{{ target }}" {{ args }}

ssh-qualify-native-cache-s3-performance-smoke target run_id remote_url bucket region prefix:
    @"{{ dev_machine }}" just cargo-rail "{{ target }}" \
      "qualify-native-cache-s3-performance-smoke" \
      "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

ssh-qualify-native-cache-s3-performance target runs run_id remote_url bucket region prefix:
    @"{{ dev_machine }}" just cargo-rail "{{ target }}" \
      "qualify-native-cache-s3-performance" \
      "{{ runs }}" "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

ssh-collect-bench target run_id destination:
    @"{{ dev_machine }}" collect-bench cargo-rail "{{ target }}" "{{ run_id }}" "{{ destination }}"

check:
    @scripts/check/check.sh

fix:
    @scripts/check/check.sh --fix

test crate="":
    @scripts/test/test.sh "{{ crate }}"

build:
    @scripts/build/build.sh

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

qualify-native-cache-s3 phase run_id remote_url:
    @scripts/ci/qualify-native-cache-s3.sh "{{ phase }}" "{{ run_id }}" "{{ remote_url }}"

qualify-native-cache-r2-faults run_id remote_url bucket:
    @scripts/ci/qualify-native-cache-r2-faults.sh "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}"

qualify-native-cache-r2-faults-resume-outage run_id remote_url bucket:
    @scripts/ci/qualify-native-cache-r2-faults.sh "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" resume-outage

qualify-native-cache-s3-performance runs run_id remote_url bucket region prefix:
    @scripts/ci/qualify-native-cache-s3-performance.sh "{{ runs }}" "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

qualify-native-cache-s3-performance-smoke run_id remote_url bucket region prefix:
    @CARGO_RAIL_PERFORMANCE_SMOKE=1 scripts/ci/qualify-native-cache-s3-performance.sh 1 "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

cleanup-native-cache-s3-prefix bucket region prefix:
    @scripts/ci/cleanup-native-cache-s3-prefix.sh "{{ bucket }}" "{{ region }}" "{{ prefix }}"

cleanup-native-cache-r2-prefix account bucket prefix:
    @scripts/ci/cleanup-native-cache-r2-prefix.sh "{{ account }}" "{{ bucket }}" "{{ prefix }}"

install-qualification-tools:
    @scripts/ci/install-qualification-tools.sh

bench-native-cache-archive run_id:
    @scripts/bench/native-cache-archive.sh "{{ run_id }}"

bench-native-cache-prune *run_ids:
    @scripts/bench/native-cache-prune.sh {{ run_ids }}

gen-fixture members output:
    @scripts/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

# CI Commands (for GitHub Actions)

check-ci:
    @scripts/check/check.sh

# Explainability

plan:
    cargo run --quiet --locked --target-dir "${RAIL_BOOTSTRAP_TARGET_DIR:-target/cargo-rail-bootstrap}" -- rail plan --merge-base -f json

# Maintenance

gen-docs:
    @scripts/docs/generate.sh

pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
