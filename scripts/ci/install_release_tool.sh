#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/ci/install_release_tool.sh <cross|tauri-cli> [--print-manifest]

Installs a SHA-256-pinned release binary into Cargo's bin directory. The
--print-manifest mode resolves the runner-specific asset without downloading it
and is used by install_release_tool.test.sh.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

normalize_os() {
  local raw="${ZEROCLAW_RELEASE_TOOL_OS:-${RUNNER_OS:-}}"
  if [[ -z "$raw" ]]; then
    raw="$(uname -s)"
  fi

  case "$raw" in
    Linux*) echo "linux" ;;
    Darwin|macOS) echo "macos" ;;
    Windows|MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
    *) die "unsupported operating system: $raw" ;;
  esac
}

normalize_arch() {
  local raw="${ZEROCLAW_RELEASE_TOOL_ARCH:-${RUNNER_ARCH:-}}"
  if [[ -z "$raw" ]]; then
    raw="$(uname -m)"
  fi

  case "$raw" in
    X64|x86_64|amd64) echo "x86_64" ;;
    ARM64|aarch64|arm64) echo "aarch64" ;;
    *) die "unsupported architecture: $raw" ;;
  esac
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$path" | awk '{ print $NF }'
  else
    die "sha256sum, shasum, or openssl is required to verify release tools"
  fi
}

extract_archive() {
  local archive="$1"
  local destination="$2"

  case "$archive" in
    *.tar.gz|*.tgz)
      tar -xzf "$archive" -C "$destination"
      ;;
    *.zip)
      if command -v unzip >/dev/null 2>&1; then
        unzip -q "$archive" -d "$destination"
      elif command -v 7z >/dev/null 2>&1; then
        7z x -y "-o${destination}" "$archive" >/dev/null
      else
        die "unzip or 7z is required to extract $archive"
      fi
      ;;
    *) die "unsupported archive format: $archive" ;;
  esac
}

tool="${1:-}"
mode="${2:-}"
if [[ -z "$tool" ]] || [[ "$mode" != "" && "$mode" != "--print-manifest" ]] || [[ $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

os="$(normalize_os)"
arch="$(normalize_arch)"

case "$tool" in
  cross)
    version="0.2.5"
    tag="v${version}"
    repository="cross-rs/cross"
    primary_binary="cross"
    binaries=("cross" "cross-util")
    case "${os}/${arch}" in
      linux/x86_64)
        asset="cross-x86_64-unknown-linux-gnu.tar.gz"
        sha256="642375d1bcf3bd88272c32ba90e999f3d983050adf45e66bd2d3887e8e838bad"
        ;;
      *) die "cross ${version} is not pinned for ${os}/${arch}" ;;
    esac
    ;;
  tauri-cli)
    version="2.11.4"
    tag="tauri-cli-v${version}"
    repository="tauri-apps/tauri"
    case "${os}/${arch}" in
      linux/x86_64)
        asset="cargo-tauri-x86_64-unknown-linux-gnu.tgz"
        primary_binary="cargo-tauri"
        binaries=("cargo-tauri")
        sha256="6864602a34292aa6f2ad40ae019eebe5c1064d6c623fe20696a8a8974067e60b"
        ;;
      macos/x86_64)
        asset="cargo-tauri-x86_64-apple-darwin.zip"
        primary_binary="cargo-tauri"
        binaries=("cargo-tauri")
        sha256="f10dfcc103ccb79248ca27cb9aff7b8a65499d1b0df79fe0465e8aa0a8e7cbef"
        ;;
      macos/aarch64)
        asset="cargo-tauri-aarch64-apple-darwin.zip"
        primary_binary="cargo-tauri"
        binaries=("cargo-tauri")
        sha256="82bdcb9ae7f407882321680ae50750f11623fae22445f8b00b096e10f815d604"
        ;;
      windows/x86_64)
        asset="cargo-tauri-x86_64-pc-windows-msvc.zip"
        primary_binary="cargo-tauri.exe"
        binaries=("cargo-tauri.exe")
        sha256="0743e30a661a35d63339b24cf63828f97ba5389a1d7f13b368a542794dd0a3f3"
        ;;
      *) die "tauri-cli ${version} is not pinned for ${os}/${arch}" ;;
    esac
    ;;
  *)
    usage >&2
    die "unknown release tool: $tool"
    ;;
esac

url="https://github.com/${repository}/releases/download/${tag}/${asset}"

if [[ "$mode" == "--print-manifest" ]]; then
  binaries_csv=""
  for binary in "${binaries[@]}"; do
    if [[ -n "$binaries_csv" ]]; then
      binaries_csv+=","
    fi
    binaries_csv+="$binary"
  done
  printf 'tool=%s\nversion=%s\nos=%s\narch=%s\nasset=%s\nprimary_binary=%s\nbinaries=%s\nsha256=%s\nurl=%s\n' \
    "$tool" "$version" "$os" "$arch" "$asset" "$primary_binary" "$binaries_csv" "$sha256" "$url"
  exit 0
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
archive="${tmp_dir}/${asset}"
extract_dir="${tmp_dir}/extract"
mkdir -p "$extract_dir"

echo "Downloading ${tool} ${version} for ${os}/${arch}"
curl --fail --location --silent --show-error \
  --connect-timeout 15 --max-time 120 --retry 3 --retry-all-errors \
  --proto '=https' --tlsv1.2 \
  "$url" -o "$archive"

actual_sha256="$(sha256_file "$archive")"
if [[ "$actual_sha256" != "$sha256" ]]; then
  die "checksum mismatch for $asset (expected $sha256, got $actual_sha256)"
fi

extract_archive "$archive" "$extract_dir"

cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
if command -v cygpath >/dev/null 2>&1; then
  cargo_home="$(cygpath -u "$cargo_home")"
fi
install_dir="${cargo_home}/bin"
mkdir -p "$install_dir"
for binary in "${binaries[@]}"; do
  source_binary="${extract_dir}/${binary}"
  [[ -f "$source_binary" ]] || die "$binary was not present in $asset"
  cp "$source_binary" "${install_dir}/${binary}"
  chmod +x "${install_dir}/${binary}"
done

"${install_dir}/${primary_binary}" --version
