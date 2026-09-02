#!/usr/bin/env bash
set -euo pipefail

profile="${1:-}"
case "$profile" in
  bench | linux-* | windows-*) ;;
  *)
    echo "usage: $0 <bench|linux-*|windows-* dev-machines profile>" >&2
    exit 2
    ;;
esac

readonly CARGO_DENY_VERSION=0.20.2
readonly CARGO_RAIL_VERSION=0.25.0
readonly HYPERFINE_VERSION=1.20.0
readonly SCCACHE_VERSION=0.17.0
readonly JQ_VERSION=1.8.2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
REPOSITORY_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
readonly REPOSITORY_ROOT
readonly CI_TOOL_ARCHIVES="$REPOSITORY_ROOT/.config/ci-tool-archives.tsv"

ci_tool_version() {
  local tool="$1"
  awk -F '\t' -v tool="$tool" '
    $0 !~ /^#/ && $1 == tool { versions[$2] = 1 }
    END {
      for (version in versions) {
        count += 1
        selected = version
      }
      if (count != 1) exit 1
      print selected
    }
  ' "$CI_TOOL_ARCHIVES"
}

ci_tool_archive() {
  local tool="$1"
  local archive_target="$2"
  awk -F '\t' -v tool="$tool" -v archive_target="$archive_target" '
    BEGIN { OFS = FS }
    $0 !~ /^#/ && $1 == tool && $3 == archive_target {
      print $2, $4
      matches += 1
    }
    END { if (matches != 1) exit 1 }
  ' "$CI_TOOL_ARCHIVES"
}

cargo_bin="$HOME/.cargo/bin"
mkdir -p "$cargo_bin"
export PATH="$cargo_bin:$PATH"
rust_host="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$rust_host" ]]; then
  echo "rustc did not report its native host target" >&2
  exit 1
fi
if [[ "$rust_host" == *-pc-windows-msvc && "${CARGO_RAIL_WINDOWS_RECIPE_SHELL_ACTIVE:-}" != 1 ]]; then
  export CARGO_RAIL_WINDOWS_RECIPE_SHELL_ACTIVE=1
  printf -v native_command '%q ' bash "$0" "$@"
  exec "$SCRIPT_DIR/windows-recipe-shell.sh" "$native_command"
fi

# dev-machines installs the remote cache environment before this script. The
# wrapper binary does not exist until sccache is installed below.
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER

install_cargo_tool() {
  local package="$1"
  local version="$2"
  local binary="${3:-$package}"
  local path="$cargo_bin/$binary"
  [[ -x "$path" ]] || path="$path.exe"
  if [[ -x "$path" ]] && "$path" --version 2>&1 | grep -Fq "$version"; then
    echo "$package $version is already installed"
    return
  fi
  cargo install --registry crates-io --locked --version "=$version" --force "$package"
  [[ -x "$cargo_bin/$binary" || -x "$cargo_bin/$binary.exe" ]] || {
    echo "$package did not install $binary" >&2
    exit 1
  }
}

extract_zip_member() {
  local archive="$1"
  local member="$2"
  local destination="$3"
  if [[ "${OS:-}" != Windows_NT ]]; then
    tar -xOf "$archive" "$member" >"$destination"
    return
  fi

  local archive_windows destination_windows
  archive_windows="$(cygpath -w "$archive")"
  destination_windows="$(cygpath -w "$destination")"
  # shellcheck disable=SC2016 # PowerShell owns interpolation in this program.
  CARGO_RAIL_TOOL_ARCHIVE="$archive_windows" \
    CARGO_RAIL_TOOL_MEMBER="$member" \
    CARGO_RAIL_TOOL_DESTINATION="$destination_windows" \
    powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command '
      $ErrorActionPreference = "Stop"
      Add-Type -AssemblyName System.IO.Compression.FileSystem
      $archive = [System.IO.Compression.ZipFile]::OpenRead($env:CARGO_RAIL_TOOL_ARCHIVE)
      try {
        $entries = @($archive.Entries | Where-Object { $_.FullName -ceq $env:CARGO_RAIL_TOOL_MEMBER })
        if ($entries.Count -ne 1) {
          throw "archive must contain exactly one $($env:CARGO_RAIL_TOOL_MEMBER) member"
        }
        $source = $entries[0].Open()
        try {
          $output = [System.IO.File]::Create($env:CARGO_RAIL_TOOL_DESTINATION)
          try {
            $source.CopyTo($output)
          } finally {
            $output.Dispose()
          }
        } finally {
          $source.Dispose()
        }
      } finally {
        $archive.Dispose()
      }
    '
}

install_release_binary() {
  local package="$1"
  local version="$2"
  local repository="$3"
  local tag="$4"
  local asset="$5"
  local digest="$6"
  local member="$7"
  local binary="${8:-$package}"
  local version_program="${9:-$package}"
  local source_url="${10:-https://github.com/$repository/releases/download/$tag/$asset}"
  local destination="$cargo_bin/$binary"
  local expected_version="$version_program $version"
  if [[ -x "$destination" ]] && [[ "$("$destination" --version 2>&1)" == "$expected_version" ]]; then
    echo "$package $version is already installed"
    return
  fi

  local temporary archive staged
  temporary="$(mktemp -d "${TMPDIR:-/tmp}/cargo-rail-tool.XXXXXX")"
  staged="$(mktemp "$cargo_bin/.${binary}.XXXXXX")"
  trap 'rm -rf -- "$temporary"; rm -f -- "$staged"' RETURN
  archive="$temporary/$asset"
  curl --proto '=https' --tlsv1.2 -fsSL \
    "$source_url" \
    -o "$archive"
  printf '%s  %s\n' "$digest" "$archive" | sha256sum --check --status
  case "$asset" in
    *.tar.gz | *.tgz) tar -xOzf "$archive" "$member" >"$staged" ;;
    *.zip) extract_zip_member "$archive" "$member" "$staged" ;;
    *)
      echo "$package archive has unsupported format: $asset" >&2
      exit 1
      ;;
  esac
  chmod +x "$staged"
  mv -f "$staged" "$destination"
  [[ "$("$destination" --version 2>&1)" == "$expected_version" ]] || {
    echo "$package archive did not install exact version $expected_version" >&2
    exit 1
  }
  rm -rf -- "$temporary"
  trap - RETURN
}

install_just() {
  local archive_target asset binary digest record source_url suffix version
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      archive_target=x86_64-unknown-linux-musl
      binary=just
      ;;
    aarch64-unknown-linux-gnu)
      archive_target=aarch64-unknown-linux-musl
      binary=just
      ;;
    x86_64-pc-windows-msvc)
      archive_target=x86_64-pc-windows-msvc
      binary=just.exe
      ;;
    aarch64-pc-windows-msvc)
      archive_target=aarch64-pc-windows-msvc
      binary=just.exe
      ;;
    *)
      echo "just has no configured asset for Rust host $rust_host" >&2
      exit 1
      ;;
  esac
  record="$(ci_tool_archive just "$archive_target")" || {
    echo "just has no unique archive pin for $archive_target" >&2
    exit 1
  }
  IFS=$'\t' read -r version digest <<<"$record"
  suffix=tar.gz
  [[ "$archive_target" == *-pc-windows-msvc ]] && suffix=zip
  asset="just-$version-$archive_target.$suffix"
  source_url="https://github.com/casey/just/releases/download/$version/$asset"
  install_release_binary just "$version" casey/just "$version" "$asset" "$digest" "$binary" "$binary" just \
    "$source_url"
}

install_hyperfine() {
  local asset digest archive_target binary
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      archive_target="x86_64-unknown-linux-gnu"
      asset="hyperfine-v$HYPERFINE_VERSION-$archive_target.tar.gz"
      digest="63ad53934062118f5b0be11785e0bb1603d4b91667d1921f2fd8df9a8712040a"
      binary=hyperfine
      ;;
    aarch64-unknown-linux-gnu)
      archive_target="aarch64-unknown-linux-gnu"
      asset="hyperfine-v$HYPERFINE_VERSION-$archive_target.tar.gz"
      digest="90875cb1db7a1d797c311174d061728361e58fc70e3b62262a00635ac3b1997c"
      binary=hyperfine
      ;;
    x86_64-pc-windows-msvc)
      archive_target="x86_64-pc-windows-msvc"
      asset="hyperfine-v$HYPERFINE_VERSION-$archive_target.zip"
      digest="2508c549b049b1d4342d08edc1cb42bfac169082b6e3069431b5bab9822dbb32"
      binary=hyperfine.exe
      ;;
    aarch64-pc-windows-msvc)
      install_cargo_tool hyperfine "$HYPERFINE_VERSION"
      return
      ;;
    *)
      echo "hyperfine $HYPERFINE_VERSION has no configured asset for Rust host $rust_host" >&2
      exit 1
      ;;
  esac
  install_release_binary hyperfine "$HYPERFINE_VERSION" sharkdp/hyperfine "v$HYPERFINE_VERSION" \
    "$asset" "$digest" "hyperfine-v$HYPERFINE_VERSION-$archive_target/$binary" "$binary"
}

install_sccache() {
  local asset digest archive_target binary
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      archive_target="x86_64-unknown-linux-musl"
      asset="sccache-v$SCCACHE_VERSION-$archive_target.tar.gz"
      digest="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
      binary=sccache
      ;;
    aarch64-unknown-linux-gnu)
      archive_target="aarch64-unknown-linux-musl"
      asset="sccache-v$SCCACHE_VERSION-$archive_target.tar.gz"
      digest="821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77"
      binary=sccache
      ;;
    x86_64-pc-windows-msvc)
      archive_target="x86_64-pc-windows-msvc"
      asset="sccache-v$SCCACHE_VERSION-$archive_target.zip"
      digest="e94cfc5b58cbe439302f586c1d1bd7980c2cd371d47bdf385ade657411e6f3ac"
      binary=sccache.exe
      ;;
    aarch64-pc-windows-msvc)
      archive_target="aarch64-pc-windows-msvc"
      asset="sccache-v$SCCACHE_VERSION-$archive_target.zip"
      digest="82994d1bc92ccc0556f7e6e0ad6cbd08a41a1e84b461fcae628ac2afc8c372bf"
      binary=sccache.exe
      ;;
    *)
      echo "sccache $SCCACHE_VERSION has no configured asset for Rust host $rust_host" >&2
      exit 1
      ;;
  esac
  install_release_binary sccache "$SCCACHE_VERSION" mozilla/sccache "v$SCCACHE_VERSION" \
    "$asset" "$digest" "sccache-v$SCCACHE_VERSION-$archive_target/$binary" "$binary"
}

install_sccache_dist() {
  local asset digest archive_target
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      archive_target="x86_64-unknown-linux-musl"
      asset="sccache-dist-v$SCCACHE_VERSION-$archive_target.tar.gz"
      digest="affec995429df5f97498e93d373b10733b0048a6391554c83ccd57d9b6c23d1c"
      ;;
    aarch64-unknown-linux-gnu)
      archive_target="aarch64-unknown-linux-musl"
      asset="sccache-dist-v$SCCACHE_VERSION-$archive_target.tar.gz"
      digest="5b84340b1e34d9c7426be3b9e4e674b6a01dc70187ad6640f83446d1ee94a0e3"
      ;;
    *)
      echo "sccache-dist $SCCACHE_VERSION has no configured asset for Rust host $rust_host" >&2
      exit 1
      ;;
  esac
  install_release_binary sccache-dist "$SCCACHE_VERSION" mozilla/sccache "v$SCCACHE_VERSION" \
    "$asset" "$digest" "sccache-dist-v$SCCACHE_VERSION-$archive_target/sccache-dist" sccache-dist sccache
}

install_cargo_nextest() {
  local archive_target asset binary digest path record source_url temporary version
  binary=cargo-nextest
  [[ "$rust_host" == *-pc-windows-msvc ]] && binary=cargo-nextest.exe

  case "$rust_host" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | x86_64-pc-windows-msvc | aarch64-pc-windows-msvc)
      archive_target="$rust_host"
      ;;
    *)
      version="$(ci_tool_version cargo-nextest)" || {
        echo "cargo-nextest does not have one configured version" >&2
        exit 1
      }
      install_cargo_tool cargo-nextest "$version" cargo-nextest
      return
      ;;
  esac

  record="$(ci_tool_archive cargo-nextest "$archive_target")" || {
    echo "cargo-nextest has no unique archive pin for $archive_target" >&2
    exit 1
  }
  IFS=$'\t' read -r version digest <<<"$record"
  asset="cargo-nextest-$version-$archive_target.tar.gz"
  source_url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/$asset"
  path="$cargo_bin/$binary"
  if [[ -x "$path" ]] && "$path" --version 2>&1 | grep -Fq "$version"; then
    echo "cargo-nextest $version is already installed"
    return
  fi

  temporary="$(mktemp "${TMPDIR:-/tmp}/cargo-nextest.XXXXXX")"
  trap 'rm -f -- "$temporary"' RETURN
  curl --proto '=https' --tlsv1.2 -fsSL "$source_url" -o "$temporary"
  printf '%s  %s\n' "$digest" "$temporary" | sha256sum --check --status
  tar -xzf "$temporary" -C "$cargo_bin" "$binary"
  chmod +x "$path"
  "$path" --version 2>&1 | grep -Fq "$version" || {
    echo "cargo-nextest archive did not install version $version" >&2
    exit 1
  }
  rm -f -- "$temporary"
  trap - RETURN
}

install_cargo_deny() {
  local archive_target asset binary digest
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      archive_target=x86_64-unknown-linux-musl
      binary=cargo-deny
      digest="9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"
      ;;
    aarch64-unknown-linux-gnu)
      archive_target=aarch64-unknown-linux-musl
      binary=cargo-deny
      digest="995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d"
      ;;
    x86_64-pc-windows-msvc)
      archive_target=x86_64-pc-windows-msvc
      binary=cargo-deny.exe
      digest="975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc"
      ;;
    *)
      install_cargo_tool cargo-deny "$CARGO_DENY_VERSION" cargo-deny
      return
      ;;
  esac
  asset="cargo-deny-$CARGO_DENY_VERSION-$archive_target.tar.gz"
  install_release_binary cargo-deny "$CARGO_DENY_VERSION" EmbarkStudios/cargo-deny "$CARGO_DENY_VERSION" \
    "$asset" "$digest" "cargo-deny-$CARGO_DENY_VERSION-$archive_target/$binary" "$binary"
}

install_jq() {
  if command -v jq >/dev/null 2>&1 && [[ "$(jq --version)" == "jq-$JQ_VERSION" ]]; then
    echo "jq $JQ_VERSION is already installed"
    return
  fi

  local asset digest destination
  case "$rust_host" in
    x86_64-unknown-linux-gnu)
      asset="jq-linux-amd64"
      digest="b1c22172dd303f3be49e935aa56aa48a8b7a46e0bc838b4997d3bb451495870f"
      destination="$cargo_bin/jq"
      ;;
    aarch64-unknown-linux-gnu)
      asset="jq-linux-arm64"
      digest="8b85c817833814ddca00a144c33705546355afccf0cf39b188f3cdb48b852309"
      destination="$cargo_bin/jq"
      ;;
    x86_64-pc-windows-msvc)
      asset="jq-windows-amd64.exe"
      digest="a6fc67fedaf9128a3309a1e2ebb8b986aeccf70122ee46d2cb4849e423f0c627"
      destination="$cargo_bin/jq.exe"
      ;;
    aarch64-pc-windows-msvc)
      asset="jq-windows-arm64.exe"
      digest="083b5377392bc57cf27052b6d20a2d927770683bca844632901ff38b4b7b0ac7"
      destination="$cargo_bin/jq.exe"
      ;;
    *)
      echo "jq $JQ_VERSION has no configured asset for Rust host $rust_host" >&2
      exit 1
      ;;
  esac
  local temporary
  temporary="$(mktemp "${TMPDIR:-/tmp}/jq.XXXXXX")"
  trap 'rm -f -- "$temporary"' RETURN
  curl --proto '=https' --tlsv1.2 -fsSL \
    "https://github.com/jqlang/jq/releases/download/jq-$JQ_VERSION/$asset" \
    -o "$temporary"
  printf '%s  %s\n' "$digest" "$temporary" | sha256sum --check --status
  mv "$temporary" "$destination"
  chmod +x "$destination"
  trap - RETURN
}

install_cargo_rail() {
  # The release installer validates and replaces the complete component set.
  # A matching CLI version alone cannot prove companion component authority.
  if [[ "$rust_host" == *-pc-windows-msvc ]]; then
    powershell.exe -NoProfile -ExecutionPolicy Bypass \
      -File scripts/install.ps1 -Version "$CARGO_RAIL_VERSION"
  else
    scripts/install.sh "$CARGO_RAIL_VERSION"
  fi
  [[ "$(cargo rail --version 2>&1)" == "cargo-rail $CARGO_RAIL_VERSION" ]] || {
    echo "cargo-rail did not install exact version $CARGO_RAIL_VERSION" >&2
    exit 1
  }
}

install_just
install_jq
install_cargo_rail

just --version
jq --version
cargo rail --version

case "$profile" in
  *-ci)
    install_cargo_nextest
    install_cargo_deny
    cargo nextest --version
    cargo deny --version
    ;;
  native-cache-qualification)
    install_cargo_nextest
    install_cargo_deny
    install_hyperfine
    install_sccache
    cargo nextest --version
    cargo deny --version
    hyperfine --version
    sccache --version
    ;;
  *-distributed-qualification)
    # Distributed qualification compares against the exact pinned sccache
    # client, scheduler, and server release on the same bounded topology.
    install_cargo_nextest
    install_cargo_deny
    install_sccache
    install_sccache_dist
    cargo nextest --version
    cargo deny --version
    sccache --version
    sccache-dist --version
    ;;
  *-qualification)
    # Benchmark images stay lean. Qualification explicitly adds the tools used
    # by `just check` and `just test-all` before measurements are retained.
    install_cargo_nextest
    install_cargo_deny
    cargo nextest --version
    cargo deny --version
    ;;
  bench | *-bench)
    install_hyperfine
    install_sccache
    hyperfine --version
    sccache --version
    ;;
  *)
    install_cargo_nextest
    install_cargo_deny
    install_hyperfine
    install_sccache
    cargo deny --version
    cargo nextest --version
    hyperfine --version
    sccache --version
    ;;
esac
