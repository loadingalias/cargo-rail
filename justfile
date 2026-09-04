check:
    @scripts/check/check.sh

check-affected:
    @scripts/plan/affected.sh

check-compiler-fact-driver:
    @scripts/check-compiler-fact-driver.sh

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

build-all:
    cargo build --workspace --all-targets --all-features --locked

bench-unify packages="25" runs="10": build-release
    @scripts/bench/unify.sh "{{ packages }}" "{{ runs }}"

bench-plan runs="20":
    @scripts/bench/plan.py run "{{ runs }}"

bench-plan-smoke:
    @scripts/bench/plan.py smoke

bench-compiler-facts runs="20":
    @scripts/bench/compiler-facts.py run "{{ runs }}"

bench-compiler-facts-smoke:
    @scripts/bench/compiler-facts.py smoke

bench-compiler-facts-summarize results:
    @scripts/bench/compiler-facts.py summarize "{{ results }}"

bench-compiler-facts-validate results:
    @scripts/bench/compiler-facts.py validate "{{ results }}"

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
