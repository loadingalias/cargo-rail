check: fix
    @scripts/check.sh local

ci-check:
    @scripts/check.sh native

check-compiler-driver:
    @scripts/check-compiler-fact-driver.sh

fix:
    @scripts/check.sh fix

test profile="default":
    #!/usr/bin/env bash
    set -euo pipefail
    profile={{quote(profile)}}
    [[ "$profile" == default || "$profile" == cranelift ]] || { echo 'unknown test profile' >&2; exit 2; }
    if [[ "$profile" == cranelift && "$(uname -s)" != Darwin ]]; then
        echo 'the Cranelift integration lane requires a native macOS host' >&2
        exit 1
    fi
    component_directory="$(
        cargo metadata --no-deps --format-version 1 --locked --offline |
            python3 -c 'import json, pathlib, sys; print((pathlib.Path(json.load(sys.stdin)["target_directory"]) / "debug").as_posix())'
    )"
    if [[ "$(uname -s)" == Darwin ]]; then
        scripts/prepare-cranelift.sh "$component_directory"
        source "$component_directory/cranelift-toolchain.env"
    fi
    scripts/check-source-installation.sh --prepare "$component_directory"
    source "$component_directory/source-installation-authority.env"
    scripts/check-compiler-fact-driver.sh --prepare "$component_directory"
    source "$component_directory/compiler-driver-authority.env"
    # Keep fixture Cargo commands out of the runner's build directory.
    unset CARGO_TARGET_DIR
    if [[ "$profile" == default ]]; then
        cargo nextest run --target-dir "$(dirname "$component_directory")" --workspace -P default --all-features --locked \
            --config-file .config/nextest.toml
        cargo test --target-dir "$(dirname "$component_directory")" --doc -p cargo-rail --all-features --locked
    else
        cargo nextest run --target-dir "$(dirname "$component_directory")" --workspace -P cranelift --all-features --locked \
            --config-file .config/nextest.toml
    fi

test-cranelift:
    @just test cranelift

build:
    #!/usr/bin/env bash
    set -euo pipefail
    component_directory="$(
        cargo metadata --no-deps --format-version 1 --locked --offline |
            python3 -c 'import json, pathlib, sys; print((pathlib.Path(json.load(sys.stdin)["target_directory"]) / "debug").as_posix())'
    )"
    scripts/check-compiler-fact-driver.sh --prepare "$component_directory"
    source "$component_directory/compiler-driver-authority.env"
    cargo build --workspace --all-targets --all-features --locked

build-release:
    #!/usr/bin/env bash
    set -euo pipefail
    component_directory="$(
        cargo metadata --no-deps --format-version 1 --locked --offline |
            python3 -c 'import json, pathlib, sys; print((pathlib.Path(json.load(sys.stdin)["target_directory"]) / "release").as_posix())'
    )"
    scripts/check-compiler-fact-driver.sh --prepare "$component_directory"
    source "$component_directory/compiler-driver-authority.env"
    cargo build --workspace --bins --all-features --release --locked

package-release output-directory: build-release
    python3 scripts/package-release.py {{quote(output-directory)}}

update:
    @scripts/update-all.sh

check-tooling:
    @scripts/tooling/check.sh
