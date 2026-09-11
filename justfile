# Local repair, shared validation, cross-compilation, and dogfooding.
check: fix ci-check
    @scripts/check-cross.sh
    cargo rail unify --check --explain

# Shared nonmutating checks, also run by the local check recipe.
ci-check:
    @scripts/check.sh

check-compiler-driver:
    @scripts/check-compiler-fact-driver.sh

fix:
    cargo fmt --all
    cargo clippy --workspace --all-targets --all-features --locked --fix --allow-dirty --allow-staged
    cargo fmt --all

test profile="default":
    #!/usr/bin/env bash
    set -euo pipefail
    # Fixtures own remote authority; machine enrollment must not supply cache hits.
    unset CARGO_RAIL_CACHE_REMOTE CARGO_RAIL_CACHE_MODE CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT
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
    if [[ "$profile" != cranelift ]]; then
        cargo nextest run --target-dir "$(dirname "$component_directory")" --workspace -P "$profile" --all-features --locked \
            --config-file .config/nextest.toml
        cargo test --target-dir "$(dirname "$component_directory")" --doc -p cargo-rail --all-features --locked
    else
        cargo nextest run --target-dir "$(dirname "$component_directory")" --workspace -P cranelift --all-features --locked \
            --config-file .config/nextest.toml
    fi

# Native local, remote-storage, and distributed cache qualification.
test-cache-host *args:
    @scripts/check-cache-host.sh {{args}}

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

# The sole performance benchmark; fixture preparation and resets are owned by the binary.
[positional-arguments]
bench *args: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    component_directory="$(
        cargo metadata --no-deps --format-version 1 --locked --offline |
            python3 -c 'import json, pathlib, sys; print((pathlib.Path(json.load(sys.stdin)["target_directory"]) / "release").as_posix())'
    )"
    exec "$component_directory/cargo-rail-bench" local "$@"
