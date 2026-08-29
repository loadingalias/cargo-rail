#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <native-musl-target> <github-environment-file>" >&2
  exit 2
}

[[ "$#" -eq 2 ]] || usage
target="$1"
environment_file="$2"
[[ -f "$environment_file" && ! -L "$environment_file" ]] || usage

repository=ghcr.io/taiki-e/rust-cross-toolchain
case "$target:$(uname -m)" in
  x86_64-unknown-linux-musl:x86_64)
    image_digest=sha256:f484a022584975c9bb413e1e18f4eff300fd437fec5610e9d84f1abd64100cdc
    interpreter=/lib/ld-musl-x86_64.so.1
    system_runtime=/usr/lib/x86_64-linux-musl/libgcc_s.so.1
    ;;
  aarch64-unknown-linux-musl:aarch64 | aarch64-unknown-linux-musl:arm64)
    image_digest=sha256:5a1aa71492e44d330c5583cae909fe56692c8c6b4d1064f9eb1fdc7fada3d98a
    interpreter=/lib/ld-musl-aarch64.so.1
    system_runtime=/usr/lib/aarch64-linux-musl/libgcc_s.so.1
    ;;
  *)
    echo "no pinned native musl toolchain for '$target' on '$(uname -m)'" >&2
    exit 2
    ;;
esac
readonly repository image_digest interpreter system_runtime

command -v sudo >/dev/null 2>&1 || {
  echo "native musl toolchain setup requires sudo" >&2
  exit 2
}
sudo -n true
packages=(binutils musl-tools)
if ! command -v docker >/dev/null 2>&1; then
  packages+=(docker.io)
fi
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  -o DPkg::Lock::Timeout=60 \
  "${packages[@]}"

docker_command=(docker)
if ! docker info >/dev/null 2>&1; then
  sudo systemctl start docker
  docker_command=(sudo -n docker)
fi
"${docker_command[@]}" info >/dev/null

image="$repository@$image_digest"
"${docker_command[@]}" pull "$image" >/dev/null
"${docker_command[@]}" image inspect "$image" \
  --format '{{join .RepoDigests "\n"}}' \
  | grep -Fx "$image" >/dev/null || {
    echo "Docker did not retain the requested musl toolchain digest" >&2
    exit 1
  }

temporary="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/cargo-rail-musl-toolchain.XXXXXX")"
container="cargo-rail-musl-${target//[^A-Za-z0-9_.-]/-}-$$"
container_created=false
cleanup() {
  if [[ "$container_created" == true ]]; then
    "${docker_command[@]}" rm -f "$container" >/dev/null 2>&1 || true
  fi
  sudo rm -rf -- "$temporary"
}
trap cleanup EXIT

"${docker_command[@]}" create --name "$container" "$image" >/dev/null
container_created=true
"${docker_command[@]}" cp "$container:/$target/." "$temporary"
sudo cp -a "$temporary/." /usr/local/

linker="/usr/local/bin/$target-gcc"
[[ -x "$linker" ]] || {
  echo "pinned musl toolchain did not install $linker" >&2
  exit 1
}
[[ -x "$interpreter" ]] || {
  echo "musl runtime did not install $interpreter" >&2
  exit 1
}
toolchain_runtime="$($linker -print-file-name=libgcc_s.so.1)"
[[ "$toolchain_runtime" == /usr/local/* && -f "$toolchain_runtime" && ! -L "$toolchain_runtime" ]] || {
  echo "pinned musl linker did not resolve one regular libgcc_s.so.1 runtime under /usr/local" >&2
  exit 1
}
sudo install -m 0644 "$toolchain_runtime" "$system_runtime"
[[ -f "$system_runtime" && ! -L "$system_runtime" ]] || {
  echo "native musl runtime installation did not produce $system_runtime" >&2
  exit 1
}
cmp -s "$toolchain_runtime" "$system_runtime" || {
  echo "native musl runtime does not match the pinned toolchain" >&2
  exit 1
}

target_upper="$(tr '[:lower:]-' '[:upper:]_' <<<"$target")"
target_lower="${target//-/_}"
{
  printf 'CARGO_TARGET_%s_LINKER=%s\n' "$target_upper" "$linker"
  printf 'CC_%s=%s\n' "$target_lower" "$linker"
  printf 'CARGO_RAIL_MUSL_TOOLCHAIN_IMAGE=%s\n' "$image"
} >>"$environment_file"

echo "Installed pinned native musl toolchain $image for $target."
echo "Runtime: $system_runtime"
