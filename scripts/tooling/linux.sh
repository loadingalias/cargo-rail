#!/usr/bin/env bash
# Shared implementation for the native Ubuntu entry points.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

platform="${1:?native platform is required}"
shift
[[ "$#" -eq 1 && ( "$1" == ci || "$1" == package || "$1" == riscv-build ) ]] || { echo "usage: scripts/tooling/$platform.sh {ci|package|riscv-build}" >&2; exit 64; }
operation="$1"
if [[ "$operation" == riscv-build && "$platform" != x86_64-linux ]]; then
  echo "riscv-build requires x86_64-linux" >&2; exit 64
fi
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
catalog_select() { python3 "$SCRIPT_DIR/catalog.py" select "$platform" "$operation" "$@"; }
# Resolve the complete selection before package installation; process substitutions do not propagate failures.
catalog_select > /dev/null
mapfile -t packages < <(catalog_select packages)
# Exact candidates come from the selected snapshot, including repeat installations.
pinned_packages=()
for package in "${packages[@]}"; do
  version="$(apt-cache "${apt_options[@]}" madison "$package" | awk 'NR == 1 {print $3}')"
  [[ -n "$version" && "$version" != '(none)' ]] || { echo "missing Ubuntu package: $package" >&2; exit 1; }
  pinned_packages+=("$package=$version")
done
"${apt[@]}" install -y --allow-downgrades --no-install-recommends "${pinned_packages[@]}"

prefix="$HOME/.local/share/cargo-rail-tooling"
mkdir -p "$prefix"
python3 "$SCRIPT_DIR/catalog.py" download "$platform" rustup "$temporary/rustup-init"
chmod +x "$temporary/rustup-init"
channel="$(python3 "$SCRIPT_DIR/catalog.py" rust-channel "$platform" "$operation")"
"$temporary/rustup-init" -y --no-modify-path --default-host "$(catalog_get "$platform" rust-host)" --default-toolchain none
export PATH="$HOME/.cargo/bin:$PATH"
mapfile -t components < <(catalog_select components)
toolchain_args=()
for component in "${components[@]}"; do toolchain_args+=(--component "$component"); done
if [[ "$operation" == riscv-build ]]; then
  toolchain_args+=(--target "$(catalog_get riscv64-linux rust-host)")
fi
rustup toolchain install "$channel" --profile minimal "${toolchain_args[@]}"
if [[ "$operation" == riscv-build ]]; then
  # Keep target compiler internals separate from the executable build-host sysroot.
  rustup toolchain install "$channel-$(catalog_get riscv64-linux rust-host)" \
    --force-non-host --profile minimal --component rustc-dev
fi
# Explicit toolchain selection avoids auto-installing rust-toolchain.toml's
# cross targets and local development components when Cargo runs in this checkout.
export RUSTUP_TOOLCHAIN="$channel"

# Archive tools retain their complete directory layouts.
python3 "$SCRIPT_DIR/catalog.py" install-archives "$platform" "$operation" "$prefix" > "$temporary/archives"
tool_paths=()
while IFS=$'\t' read -r _name directory; do
  if [[ -d "$directory/bin" ]]; then tool_paths+=("$directory/bin"); else tool_paths+=("$directory"); fi
done < "$temporary/archives"
tool_paths+=("$HOME/.cargo/bin")
PATH="$(IFS=:; echo "${tool_paths[*]}"):$PATH"
export PATH
mapfile -t cargo_tools < <(catalog_select cargo)
for tool in "${cargo_tools[@]}"; do
  version="$(catalog_get cargo "$tool")"
  # Cargo's install registry verifies exact installed package versions on reruns.
  env -u RUSTC_WRAPPER -u CARGO_ENCODED_RUSTFLAGS \
    cargo +"$channel" binstall --locked --no-confirm --targets "$(catalog_get "$platform" rust-host)" "$tool@$version"
done
python3 "$SCRIPT_DIR/verify.py" "$platform" "$operation"
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
printf 'Installed %s %s tooling. New shells load %s.\n' "$platform" "$operation" "$environment"
