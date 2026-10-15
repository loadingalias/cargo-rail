#!/usr/bin/env bash
# Shared implementation for the native Ubuntu entry points.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

platform="${1:?native platform is required}"
shift
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# uv runs every Python command with the interpreter and packages that the repository pins.
export UV_PROJECT="$REPO_ROOT" UV_LOCKED=1
# Any host, including the macOS workstation, can name the inputs of an installation.
if [[ "$#" -eq 2 && "$1" == fingerprint ]]; then
  exec uv run python "$SCRIPT_DIR/fingerprint.py" "$platform" "$2"
fi
[[ ( "$#" -eq 1 && ( "$1" == ci || "$1" == package || "$1" == cache-host ) ) || ( "$#" -eq 2 && "$1" == cross-build ) ]] || {
  echo "usage: scripts/tooling/$platform.sh {ci|package|cache-host|cross-build TARGET-PLATFORM|fingerprint OPERATION}" >&2; exit 64;
}
operation="$1"
cross_platform="${2:-}"
if [[ "$operation" == cross-build && "$platform" != x86_64-linux ]]; then
  echo "cross-build requires x86_64-linux" >&2; exit 64
fi
case "$platform" in
  aarch64-linux|x86_64-linux|riscv64-linux|s390x-linux) machine="${platform%-linux}" ;;
  powerpc64le-linux) machine=ppc64le ;;
  *) echo "unsupported Linux platform: $platform" >&2; exit 64 ;;
esac
[[ "$(uname -s)" == Linux && "$(uname -m)" == "$machine" ]] || {
  echo "$platform requires a native ${platform%-linux} Linux host" >&2; exit 1;
}
cd "$REPO_ROOT"
# Read the required release before uv provides the full TOML reader.
catalog="$REPO_ROOT/.config/tooling.toml"
bootstrap_value() { sed -n '/^\['"${2:-linux}"'\]$/,/^\[/s/^'"$1"' = "\([^"]*\)"$/\1/p' "$catalog"; }
ubuntu="$(bootstrap_value ubuntu "$platform")"
ubuntu="${ubuntu:-$(bootstrap_value ubuntu)}"
# shellcheck source=/dev/null
source /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == "$ubuntu" ]] || {
  echo "expected Ubuntu $ubuntu; found $PRETTY_NAME" >&2; exit 1;
}
sudo_cmd=()
if [[ "$(id -u)" != 0 ]]; then sudo_cmd=(sudo); fi
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
# Ubuntu packages come from the guest's own archive; a baked image pins them.
apt=("${sudo_cmd[@]}" env DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 -o APT::Update::Error-Mode=any)
apt_updated=false
apt_update() {
  [[ "$apt_updated" == true ]] || "${apt[@]}" update
  apt_updated=true
}
# Minimal Ubuntu images may omit curl and the HTTPS trust store.
if ! command -v curl >/dev/null || [[ ! -f /etc/ssl/certs/ca-certificates.crt ]]; then
  apt_update
  "${apt[@]}" install -y curl ca-certificates
fi
uv_directory="$("$SCRIPT_DIR/uv.sh" "$platform")"
export PATH="$uv_directory:$PATH"
catalog_get() { uv run python "$SCRIPT_DIR/catalog.py" get "$@"; }
catalog_select() { uv run python "$SCRIPT_DIR/catalog.py" select "$platform" "$operation" "$@"; }
build_jobs="$(catalog_get "$platform" cargo-build-jobs 2>/dev/null || true)"
if [[ -n "$build_jobs" ]]; then export CARGO_BUILD_JOBS="$build_jobs"; fi
# Resolve the complete selection first; process substitutions do not propagate failures.
catalog_select > /dev/null
prefix="$HOME/.local/share/cargo-rail-tooling"
mkdir -p "$prefix"
environment="$prefix/environment.sh"
host="$(catalog_get "$platform" rust-host)"
channel="$(uv run python "$SCRIPT_DIR/catalog.py" rust-channel "$platform" "$operation" ${cross_platform:+"$cross_platform"})"
export PATH="$HOME/.cargo/bin:$PATH"
mapfile -t cargo_tools < <(catalog_select cargo)
# Each tool reports its own version through the selected toolchain, which nothing installs implicitly.
pinned() { RUSTUP_AUTO_INSTALL=0 RUSTUP_TOOLCHAIN="$channel" uv run python "$SCRIPT_DIR/catalog.py" require "$@"; }

# Archive tools retain their complete directory layouts. An installed archive is found by its receipt.
install_archives() {
  uv run python "$SCRIPT_DIR/catalog.py" install-archives "$platform" "$operation" "$prefix" > "$temporary/archives"
  tool_paths=("$uv_directory")
  while IFS=$'\t' read -r _name directory; do
    if [[ -d "$directory/bin" ]]; then tool_paths+=("$directory/bin"); else tool_paths+=("$directory"); fi
  done < "$temporary/archives"
  tool_paths+=("$HOME/.cargo/bin")
  PATH="$(IFS=:; echo "${tool_paths[*]}"):$PATH"
  export PATH
}
# Persistent paths are shared by interactive shells and non-interactive Bash recipes.
publish_environment() {
  {
    printf "export PATH=%q:\"\$PATH\"\n" "$(IFS=:; echo "${tool_paths[*]}")"
    printf 'export RUSTUP_TOOLCHAIN=%q\n' "$channel"
    if [[ -n "$build_jobs" ]]; then printf 'export CARGO_BUILD_JOBS=%q\n' "$build_jobs"; fi
  } > "$environment"
  if [[ -n "${GITHUB_PATH:-}" ]]; then printf '%s\n' "${tool_paths[@]}" >> "$GITHUB_PATH"; fi
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    printf 'RUSTUP_TOOLCHAIN=%s\n' "$channel" >> "$GITHUB_ENV"
    if [[ -n "$build_jobs" ]]; then printf 'CARGO_BUILD_JOBS=%s\n' "$build_jobs" >> "$GITHUB_ENV"; fi
  fi
  for startup in "$HOME/.profile" "$HOME/.bashrc"; do
    line="source \"$environment\""
    touch "$startup"
    grep -Fxq "$line" "$startup" || printf '\n%s\n' "$line" >> "$startup"
  done
}
# A complete run records the fingerprint of its inputs. While it matches, a converged
# host or image republishes its environment for this operation once the pins answer.
fingerprint=""
if [[ "$operation" != cross-build ]]; then
  fingerprint="$(uv run python "$SCRIPT_DIR/fingerprint.py" "$platform" "$operation")"
fi
if [[ -n "$fingerprint" && -f "$prefix/fingerprint.$operation" && "$(<"$prefix/fingerprint.$operation")" == "$fingerprint" ]]; then
  install_archives
  if RUSTUP_AUTO_INSTALL=0 rustc +"$channel" -vV >/dev/null 2>&1 && pinned rustup "${cargo_tools[@]}"; then
    publish_environment
    printf 'Tooling fingerprint %s matches; published %s.\n' "$fingerprint" "$environment"
    exit 0
  fi
  echo "Tooling fingerprint $fingerprint matches, but a pinned tool is missing; converging." >&2
fi

uv run python "$SCRIPT_DIR/catalog.py" validate
mapfile -t packages < <(catalog_select packages)
# Ubuntu's perf dispatcher selects the complete running kernel release, including flavor.
# A container shares its host's kernel, whose tools belong to that host.
if [[ ! -f /.dockerenv && ! -f /run/.containerenv ]]; then
  for package in "${packages[@]}"; do
    if [[ "$package" == linux-tools-common ]]; then
      packages+=("linux-tools-$(uname -r)")
      break
    fi
  done
fi
# A host that already holds every package needs no archive update at all.
# shellcheck disable=SC2016 # dpkg-query expands its own format fields.
installed="$(dpkg-query -W -f='${db:Status-Abbrev}${Package}\n' "${packages[@]}" 2>/dev/null || true)"
if [[ "$(awk '$1 == "ii" { print $2 }' <<<"$installed" | sort -u | wc -l)" -eq "$(printf '%s\n' "${packages[@]}" | sort -u | wc -l)" ]]; then
  echo "Ubuntu packages are installed."
else
  apt_update
  "${apt[@]}" install -y --no-install-recommends "${packages[@]}"
fi

# rustup reports its own version; only a missing or different pin is installed.
if ! pinned rustup >/dev/null 2>&1; then
  uv run python "$SCRIPT_DIR/catalog.py" download "$platform" rustup "$temporary/rustup-init"
  chmod +x "$temporary/rustup-init"
  "$temporary/rustup-init" -y --no-modify-path --default-host "$host" --default-toolchain none
fi
mapfile -t components < <(catalog_select components)
toolchain_args=()
for component in "${components[@]}"; do toolchain_args+=(--component "$component"); done
mapfile -t targets < <(catalog_select targets)
for target in "${targets[@]}"; do toolchain_args+=(--target "$target"); done
if [[ "$operation" == cross-build ]]; then
  toolchain_args+=(--target "$(catalog_get "$cross_platform" rust-host)")
fi
rustup toolchain install "$channel" --profile minimal "${toolchain_args[@]}"
if [[ "$operation" == cross-build ]]; then
  # Keep target compiler internals separate from the executable build-host sysroot.
  rustup toolchain install "$channel-$(catalog_get "$cross_platform" rust-host)" \
    --force-non-host --profile minimal --component rustc-dev
fi
# Explicit toolchain selection avoids auto-installing rust-toolchain.toml's
# cross targets and local development components when Cargo runs in this checkout.
export RUSTUP_TOOLCHAIN="$channel"

install_archives
for tool in "${cargo_tools[@]}"; do
  # Each tool reports its own version; only a missing or different pin is installed.
  if pinned "$tool" >/dev/null 2>&1; then continue; fi
  version="$(catalog_get cargo "$tool")"
  if command -v cargo-binstall >/dev/null; then
    env -u RUSTC_WRAPPER -u CARGO_ENCODED_RUSTFLAGS \
      cargo +"$channel" binstall --locked --no-confirm --targets "$host" "$tool@$version"
  else
    # No cargo-binstall release exists for this host; build the pinned version from its locked sources.
    env -u RUSTC_WRAPPER -u CARGO_ENCODED_RUSTFLAGS \
      cargo +"$channel" install --locked "$tool" --version "$version"
  fi
done
uv run python "$SCRIPT_DIR/verify.py" "$platform" "$operation" ${cross_platform:+"$cross_platform"}
publish_environment
[[ -z "$fingerprint" ]] || printf '%s\n' "$fingerprint" > "$prefix/fingerprint.$operation"
printf 'Installed %s %s tooling. New shells load %s.\n' "$platform" "$operation" "$environment"
