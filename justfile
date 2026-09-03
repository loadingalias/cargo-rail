[windows]
set shell := ["bash", "--noprofile", "--norc", "scripts/ci/windows-recipe-shell.sh"]

# Remote development. Provider mechanics and credentials stay in ~/dev-machines.

operator_home := env_var_or_default("HOME", env_var_or_default("USERPROFILE", "."))
dev_machine := env_var_or_default("DEV_MACHINE_BIN", operator_home + "/dev-machines/dev-machine")

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

ssh-qualification-tools target variant="":
    @"{{ dev_machine }}" just cargo-rail "{{ target }}" "install-qualification-tools" "{{ variant }}"

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

ssh-collect-compiler-facts target run_id destination:
    @"{{ dev_machine }}" collect-results cargo-rail "{{ target }}" compiler-facts "{{ run_id }}" "{{ destination }}"

ssh-collect-distributed-execution target run_id destination:
    @"{{ dev_machine }}" collect-results cargo-rail "{{ target }}" distributed-execution "{{ run_id }}" "{{ destination }}"

ssh-collect-musl-surface target run_id destination:
    @"{{ dev_machine }}" collect-results cargo-rail "{{ target }}" musl-surface "{{ run_id }}" "{{ destination }}"

check:
    @scripts/check/check.sh

check-affected:
    @scripts/plan/affected.sh

quality-affected:
    @scripts/check/check.sh --affected

check-compiler-fact-driver:
    @scripts/check-compiler-fact-driver.sh

qualify-musl-surface run_id:
    @scripts/ci/qualify-musl-surface.sh "{{ run_id }}"

check-windows-targets:
    cargo xwin check --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc
    cargo xwin check --workspace --all-targets --all-features --locked --target aarch64-pc-windows-msvc

fix:
    @scripts/check/check.sh --fix

test crate="":
    @scripts/cargo/run.sh test "{{ crate }}"

build:
    @scripts/cargo/run.sh build

build-release:
    cargo build --workspace --all-targets --all-features --release --locked

# Full Workspace Commands (no change detection)

test-all:
    @scripts/cargo/run.sh test --all

test-riscv:
    @CARGO_RAIL_TEST_MODE=riscv-ci scripts/cargo/run.sh test --all

build-all:
    cargo build --workspace --all-targets --all-features --locked

bench-unify packages="25" runs="10": build-release
    @scripts/bench/unify.sh "{{ packages }}" "{{ runs }}"

bench-plan runs="20":
    @scripts/bench/plan.py run "{{ runs }}"

bench-plan-smoke:
    @scripts/bench/plan.py smoke

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

qualify-native-cache-native-plan target:
    @scripts/bench/remote-native-cache.sh plan "{{ target }}"

qualify-native-cache-native-smoke target execute:
    @scripts/bench/remote-native-cache.sh smoke "{{ target }}" "{{ execute }}"

qualify-native-cache-native target runs execute:
    @scripts/bench/remote-native-cache.sh run "{{ target }}" "{{ runs }}" "{{ execute }}"

bench-native-cache-remote mode runs run_id:
    @scripts/bench/native-cache-remote-dispatch.sh "{{ mode }}" "{{ runs }}" "{{ run_id }}"

qualify-native-cache-s3 phase run_id remote_url:
    @scripts/ci/qualify-native-cache-s3.sh "{{ phase }}" "{{ run_id }}" "{{ remote_url }}"

validate-native-cache-remote-pair producer consumer output:
    @scripts/ci/validate-native-cache-remote-pair.py validate "{{ producer }}" "{{ consumer }}" "{{ output }}"

qualify-native-cache-remote-faults run_id remote_url:
    @scripts/ci/qualify-native-cache-remote-faults.sh "{{ run_id }}" "{{ remote_url }}"

qualify-native-cache-remote-faults-resume-outage run_id remote_url:
    @scripts/ci/qualify-native-cache-remote-faults.sh "{{ run_id }}" "{{ remote_url }}" resume-outage

qualify-native-cache-s3-performance runs run_id remote_url bucket region prefix:
    @scripts/ci/qualify-native-cache-s3-performance.sh "{{ runs }}" "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

qualify-native-cache-s3-performance-smoke run_id remote_url bucket region prefix:
    @CARGO_RAIL_PERFORMANCE_SMOKE=1 scripts/ci/qualify-native-cache-s3-performance.sh 1 "{{ run_id }}" "{{ remote_url }}" "{{ bucket }}" "{{ region }}" "{{ prefix }}"

cleanup-native-cache-s3-prefix bucket region prefix:
    @scripts/ci/cleanup-native-cache-s3-prefix.sh "{{ bucket }}" "{{ region }}" "{{ prefix }}"

cleanup-native-cache-r2-prefix account bucket prefix:
    @scripts/ci/cleanup-native-cache-r2-prefix.sh "{{ account }}" "{{ bucket }}" "{{ prefix }}"

cleanup-native-cache-azure-container account container run_id:
    @scripts/ci/cleanup-native-cache-azure-container.sh "{{ account }}" "{{ container }}" "{{ run_id }}"

install-qualification-tools variant="":
    @scripts/ci/install-qualification-tools.sh {{ variant }}

qualify-distributed-execution-prepare run_id:
    @scripts/ci/qualify-distributed-execution-node.sh prepare "{{ run_id }}"

qualify-distributed-execution-seal-identity run_id role:
    @scripts/ci/qualify-distributed-execution-node.sh seal-identity "{{ run_id }}" "{{ role }}"

qualify-distributed-execution-build run_id:
    @scripts/ci/qualify-distributed-execution-node.sh build "{{ run_id }}"

qualify-distributed-execution-resources run_id:
    @scripts/ci/qualify-distributed-execution-node.sh resources "{{ run_id }}"

qualify-distributed-execution-worker-start run_id port network="tailscale":
    @scripts/ci/qualify-distributed-execution-node.sh worker-start "{{ run_id }}" "{{ port }}" "{{ network }}"

qualify-distributed-execution-worker-stop run_id:
    @scripts/ci/qualify-distributed-execution-node.sh worker-stop "{{ run_id }}"

qualify-distributed-execution-sccache-scheduler-start run_id port network="tailscale":
    @scripts/ci/qualify-distributed-execution-node.sh sccache-scheduler-start \
      "{{ run_id }}" "{{ port }}" "{{ network }}"

qualify-distributed-execution-sccache-worker-start run_id port scheduler_endpoint network="tailscale":
    @scripts/ci/qualify-distributed-execution-node.sh sccache-worker-start \
      "{{ run_id }}" "{{ port }}" "{{ scheduler_endpoint }}" "{{ network }}"

qualify-distributed-execution-sccache-client-prepare run_id scheduler_endpoint:
    @scripts/ci/qualify-distributed-execution-node.sh sccache-client-prepare \
      "{{ run_id }}" "{{ scheduler_endpoint }}"

qualify-distributed-execution-sccache-stop run_id role:
    @scripts/ci/qualify-distributed-execution-node.sh sccache-stop "{{ run_id }}" "{{ role }}"

qualify-distributed-execution-reset-client run_id outcome:
    @scripts/ci/qualify-distributed-execution-node.sh reset-client "{{ run_id }}" "{{ outcome }}"

qualify-distributed-execution-reset-measure run_id attempt:
    @scripts/ci/qualify-distributed-execution-node.sh reset-measure "{{ run_id }}" "{{ attempt }}"

qualify-distributed-execution-run run_id outcome endpoint remote_url capability_id:
    @scripts/ci/qualify-distributed-execution-node.sh run \
      "{{ run_id }}" "{{ outcome }}" "{{ endpoint }}" "{{ remote_url }}" "{{ capability_id }}"

# Operator-bounded qualification; three-round p95 is the maximum observed sample.
qualify-distributed-execution-measure run_id rounds endpoint capability_id:
    @scripts/ci/qualify-distributed-execution-node.sh measure \
      "{{ run_id }}" "{{ rounds }}" "{{ endpoint }}" "{{ capability_id }}"

qualify-distributed-execution-report run_id:
    @scripts/ci/qualify-distributed-execution-node.sh report "{{ run_id }}"

bench-distributed-execution-archive run_id:
    @scripts/bench/distributed-execution-archive.sh "{{ run_id }}"

bench-native-cache-archive run_id:
    @scripts/bench/native-cache-archive.sh "{{ run_id }}"

bench-native-cache-prune *run_ids:
    @scripts/bench/native-cache-prune.sh {{ run_ids }}

bench-compiler-facts runs="20":
    @scripts/bench/compiler-facts.py run "{{ runs }}"

bench-compiler-facts-smoke:
    @scripts/bench/compiler-facts.py smoke

bench-compiler-facts-summarize results:
    @scripts/bench/compiler-facts.py summarize "{{ results }}"

bench-compiler-facts-validate results:
    @scripts/bench/compiler-facts.py validate "{{ results }}"

qualify-compiler-facts-remote mode runs run_id:
    @scripts/ci/qualify-compiler-facts.sh "{{ mode }}" "{{ runs }}" "{{ run_id }}"

bench-compiler-facts-archive run_id:
    @scripts/bench/compiler-facts-archive.sh "{{ run_id }}"

bench-compiler-facts-remote-plan target:
    @scripts/bench/remote-compiler-facts.sh plan "{{ target }}"

bench-compiler-facts-remote-smoke target:
    @scripts/bench/remote-compiler-facts.sh smoke "{{ target }}" --execute

bench-compiler-facts-remote target runs="20":
    @scripts/bench/remote-compiler-facts.sh run "{{ target }}" "{{ runs }}" --execute

gen-fixture members output:
    @scripts/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

# Explainability

plan:
    @scripts/plan/read.py create -

unify:
    @cargo rail unify --check --explain --show-diff

surface:
    @cargo rail surface --check --explain

cache-status:
    @cargo rail cache status

rail-cache-setup *args="":
    @scripts/cache/setup.sh {{ args }}

changes:
    @cargo rail change status

release-check:
    @cargo rail release check cargo-rail

release-status:
    @cargo rail release status --history

# Maintenance

gen-docs:
    @scripts/docs/generate.sh

pin-actions:
    @scripts/ci/pin-actions.sh --update-lock

verify-actions:
    @scripts/ci/pin-actions.sh --verify-only
