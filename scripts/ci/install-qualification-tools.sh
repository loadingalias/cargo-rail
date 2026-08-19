#!/usr/bin/env bash
set -euo pipefail

# Detects the qualification profile for the current host and installs its
# tools via install-tools.sh. Remote qualification machines are always Linux
# or Windows; this script only selects which profile applies. The optional
# variant selects a qualification family that needs extra measured tools.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

variant="${1:-}"
case "$variant" in
  "") suffix=qualification ;;
  distributed) suffix=distributed-qualification ;;
  *)
    echo "usage: $0 [distributed]" >&2
    exit 2
    ;;
esac

host="$(rustc -vV | sed -n 's/^host: //p')"
case "$host" in
  *-unknown-linux-*) profile="linux-$suffix" ;;
  *-pc-windows-*) profile="windows-$suffix" ;;
  *)
    echo "remote qualification requires a supported Linux or Windows host, got: $host" >&2
    exit 2
    ;;
esac

if [[ "$variant" == distributed && "$host" == *-unknown-linux-* ]]; then
  if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update
    packages=(bubblewrap)
    ubuntu=false
    if [[ -r /etc/os-release ]] && grep -Eq '^ID="?ubuntu"?$' /etc/os-release; then
      # Ubuntu confines unprivileged user namespaces by default. Its optional
      # profile grants Bubblewrap only the namespace setup capabilities it
      # needs while retaining AppArmor enforcement for the sandboxed child.
      packages+=(apparmor-profiles)
      ubuntu=true
    fi
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y "${packages[@]}"
    if [[ "$ubuntu" == true ]]; then
      packaged_profile=/usr/share/apparmor/extra-profiles/bwrap-userns-restrict
      apparmor_profile=/etc/apparmor.d/bwrap-userns-restrict
      disabled=/etc/apparmor.d/disable/bwrap-userns-restrict
      packaged_mode="$(stat -c '%a' "$packaged_profile" 2>/dev/null || true)"
      [[ -f "$packaged_profile" && ! -L "$packaged_profile" \
        && "$(stat -c '%u' "$packaged_profile")" == 0 \
        && $((8#$packaged_mode & 022)) -eq 0 ]] || {
        echo "distributed qualification requires Ubuntu's packaged Bubblewrap AppArmor profile" >&2
        exit 1
      }
      if [[ -e "$apparmor_profile" || -L "$apparmor_profile" ]]; then
        if [[ ! -f "$apparmor_profile" || -L "$apparmor_profile" ]] \
          || ! cmp -s "$packaged_profile" "$apparmor_profile"; then
          echo "distributed qualification refuses a modified Bubblewrap AppArmor profile" >&2
          exit 1
        fi
      else
        sudo install -o root -g root -m 0644 "$packaged_profile" "$apparmor_profile"
      fi
      if [[ -L "$disabled" ]]; then
        [[ "$(readlink "$disabled")" == "$apparmor_profile" \
          || "$(readlink -f "$disabled")" == "$apparmor_profile" ]] || {
          echo "distributed qualification found an unexpected disabled Bubblewrap profile" >&2
          exit 1
        }
        sudo unlink "$disabled"
      elif [[ -e "$disabled" ]]; then
        echo "distributed qualification found an invalid disabled Bubblewrap profile" >&2
        exit 1
      fi
      sudo apparmor_parser -r "$apparmor_profile"
    fi
  elif ! command -v bwrap >/dev/null 2>&1 && command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y bubblewrap
  elif ! command -v bwrap >/dev/null 2>&1; then
    echo "distributed qualification requires Bubblewrap, but this host has no supported package manager" >&2
    exit 1
  fi
fi

if [[ "$variant" == distributed && "$host" == *-unknown-linux-* ]]; then
  command -v bwrap >/dev/null 2>&1 || {
    echo "distributed qualification did not install Bubblewrap" >&2
    exit 1
  }
  bwrap --version
fi

exec "$script_dir/install-tools.sh" "$profile"
