#!/usr/bin/env bash
# Shared implementation for the native Ubuntu entry points.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

platform="${1:?native platform is required}"
shift
[[ "$#" -eq 0 ]] || { echo "usage: scripts/tooling/$platform.sh" >&2; exit 64; }
case "$platform" in
  aarch64-linux|x86_64-linux|riscv64-linux|s390x-linux) machine="${platform%-linux}" ;;
  powerpc64le-linux) machine=ppc64le ;;
  *) echo "unsupported Linux platform: $platform" >&2; exit 64 ;;
esac
[[ "$(uname -s)" == Linux && "$(uname -m)" == "$machine" ]] || {
  echo "$platform requires a native ${platform%-linux} Linux host" >&2; exit 1;
}
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"
# Ubuntu Server supplies Python; install it from the selected archive if absent.
# Read bootstrap strings before the full TOML reader is available.
catalog="$REPO_ROOT/.config/tooling.toml"
bootstrap_value() { sed -n '/^\['"${2:-linux}"'\]$/,/^\[/s/^'"$1"' = "\([^"]*\)"$/\1/p' "$catalog"; }
ubuntu="$(bootstrap_value ubuntu "$platform")"
ubuntu="${ubuntu:-$(bootstrap_value ubuntu)}"
snapshot="$(bootstrap_value snapshot)"
# shellcheck source=/dev/null
source /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == "$ubuntu" ]] || {
  echo "expected Ubuntu $ubuntu; found $PRETTY_NAME" >&2; exit 1;
}
sudo_cmd=()
if [[ "$(id -u)" != 0 ]]; then sudo_cmd=(sudo); fi
temporary="$(mktemp -d)"
trap '"${sudo_cmd[@]}" rm -rf -- "$temporary"' EXIT
# Minimal Ubuntu images may omit the HTTPS trust store and Python. Bootstrap
# those through Ubuntu's signed archive, then converge them to the snapshot too.
if ! command -v python3 >/dev/null || [[ ! -f /etc/ssl/certs/ca-certificates.crt ]]; then
  "${sudo_cmd[@]}" apt-get -o APT::Update::Error-Mode=any update
  "${sudo_cmd[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y python3 ca-certificates
fi
codename="$(bootstrap_value codename "$platform")"
codename="${codename:-$(bootstrap_value codename)}"
mkdir -p "$temporary/lists/partial"
chmod 755 "$temporary" "$temporary/lists" "$temporary/lists/partial"
# Explicit snapshot URLs work on an empty package cache and on all supported architectures.
# The archive remains signed; historical snapshots intentionally outlive Valid-Until.
for suite in "$codename" "$codename-updates" "$codename-security"; do
  printf 'deb [target=Packages check-valid-until=no signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://snapshot.ubuntu.com/ubuntu/%s %s main universe\n' "$snapshot" "$suite"
done > "$temporary/sources.list"
apt_options=(-o "Dir::Etc::sourcelist=$temporary/sources.list" -o Dir::Etc::sourceparts=-
  -o "Dir::State::lists=$temporary/lists" -o APT::Update::Error-Mode=any
  -o Acquire::Retries=3 -o Acquire::https::Timeout=30 -o Acquire::Languages=none)
apt=("${sudo_cmd[@]}" env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}")
"${apt[@]}" update
catalog_get() { python3 "$SCRIPT_DIR/catalog.py" get "$@"; }
python3 "$SCRIPT_DIR/catalog.py" validate
mapfile -t packages < <(catalog_get "$platform" packages)
# Exact candidates come from the selected snapshot, including repeat installations.
pinned_packages=()
for package in "${packages[@]}"; do
  version="$(apt-cache "${apt_options[@]}" madison "$package" | awk 'NR == 1 {print $3}')"
  [[ -n "$version" && "$version" != '(none)' ]] || { echo "missing Ubuntu package: $package" >&2; exit 1; }
  pinned_packages+=("$package=$version")
done
"${apt[@]}" install -y --allow-downgrades --no-install-recommends "${pinned_packages[@]}"

prefix="$HOME/.local/share/cargo-rail-tooling"
mkdir -p "$prefix/bin"
python3 "$SCRIPT_DIR/catalog.py" download "$platform" rustup "$temporary/rustup-init"
chmod +x "$temporary/rustup-init"
channel="$(python3 "$SCRIPT_DIR/catalog.py" rust-channel "$platform")"
"$temporary/rustup-init" -y --no-modify-path --default-host "$(catalog_get "$platform" rust-host)" --default-toolchain none
export PATH="$HOME/.cargo/bin:$PATH"
mapfile -t components < <(catalog_get "$platform" components)
component_args=()
for component in "${components[@]}"; do component_args+=(--component "$component"); done
rustup toolchain install "$channel" --profile minimal "${component_args[@]}"
# Explicit toolchain selection avoids auto-installing rust-toolchain.toml's
# cross targets and local development components when Cargo runs in this checkout.
export RUSTUP_TOOLCHAIN="$channel"

# Archive tools retain their complete directory layouts.
python3 "$SCRIPT_DIR/catalog.py" install-archives "$platform" "$prefix" > "$temporary/archives"
tool_paths=()
while IFS=$'\t' read -r _name directory; do
  if [[ -d "$directory/bin" ]]; then tool_paths+=("$directory/bin"); else tool_paths+=("$directory"); fi
done < "$temporary/archives"
tool_paths+=("$prefix/bin" "$HOME/.cargo/bin")
PATH="$(IFS=:; echo "${tool_paths[*]}"):$PATH"
export PATH
mapfile -t cargo_tools < <(catalog_get "$platform" cargo)
for tool in "${cargo_tools[@]}"; do
  version="$(catalog_get cargo "$tool")"
  # Cargo's install registry verifies exact installed package versions on reruns.
  env -u RUSTC_WRAPPER -u CARGO_ENCODED_RUSTFLAGS \
    cargo +"$channel" binstall --locked --no-confirm --targets "$(catalog_get "$platform" rust-host)" "$tool@$version"
done
python3 "$SCRIPT_DIR/verify.py" "$platform"
# Persistent paths are shared by interactive shells and non-interactive Bash recipes.
environment="$prefix/environment.sh"
{
  printf "export PATH=%q:\"\$PATH\"\n" "$(IFS=:; echo "${tool_paths[*]}")"
  printf 'export RUSTUP_TOOLCHAIN=%q\n' "$channel"
} > "$environment"
if [[ -n "${GITHUB_PATH:-}" ]]; then printf '%s\n' "${tool_paths[@]}" >> "$GITHUB_PATH"; fi
if [[ -n "${GITHUB_ENV:-}" ]]; then printf 'RUSTUP_TOOLCHAIN=%s\n' "$channel" >> "$GITHUB_ENV"; fi
for startup in "$HOME/.profile" "$HOME/.bashrc"; do
  line="source \"$environment\""
  touch "$startup"
  grep -Fxq "$line" "$startup" || printf '\n%s\n' "$line" >> "$startup"
done
printf 'Installed %s tooling. New shells load %s.\n' "$platform" "$environment"
case "$platform" in
  riscv64-linux|s390x-linux|powerpc64le-linux) printf 'Run scripts/check-cache-host.sh for native cache qualification.\n' ;;
  *) printf 'Run scripts/check-native-tests.sh for the complete native test suite.\n' ;;
esac
