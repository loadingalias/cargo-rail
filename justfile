check:
    @scripts/check/check.sh

check-compiler-driver:
    @scripts/check-compiler-fact-driver.sh

check-windows-targets:
    cargo xwin check --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc
    cargo xwin check --workspace --all-targets --all-features --locked --target aarch64-pc-windows-msvc

fix:
    @scripts/check/check.sh --fix

test:
    cargo nextest run --workspace -P default --all-features --locked \
        --config-file .config/nextest.toml
    cargo test --doc -p cargo-rail --all-features --locked

build:
    cargo build --workspace --all-targets --all-features --locked

build-release:
    cargo build --workspace --all-targets --all-features --release --locked

bench-unify packages="25" runs="10": build-release
    @scripts/bench/unify.sh "{{ packages }}" "{{ runs }}"

bench-compiler-facts runs="20":
    @cargo xtask compiler-facts run "{{ runs }}"

bench-compiler-facts-smoke:
    @cargo xtask compiler-facts smoke

bench-compiler-facts-summarize results:
    @cargo xtask compiler-facts summarize "{{ results }}"

bench-compiler-facts-validate results:
    @cargo xtask compiler-facts validate "{{ results }}"

gen-fixture members output:
    @tests/fixtures/generate-workspace.sh "{{ members }}" "{{ output }}"

unify:
    @cargo rail unify --check --explain --show-diff

surface:
    @cargo rail surface --check --explain

cache-status:
    @cargo rail cache status
