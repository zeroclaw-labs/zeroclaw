#!/usr/bin/env bash
set -euo pipefail

# Install pinned gitleaks binary into a writable bin directory.
# Usage: ./scripts/ci/install_gitleaks.sh <bin_dir> [version]

BIN_DIR="${1:-${RUNNER_TEMP:-/tmp}/bin}"
VERSION="${2:-${GITLEAKS_VERSION:-v8.24.2}}"

os_name="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$os_name" in
  linux|darwin) ;;
  *)
    echo "Unsupported OS for gitleaks installer: ${os_name}" >&2
    exit 2
    ;;
esac

arch_name="$(uname -m)"
case "$arch_name" in
  x86_64|amd64) arch_name="x64" ;;
  aarch64|arm64) arch_name="arm64" ;;
  armv7l) arch_name="armv7" ;;
  armv6l) arch_name="armv6" ;;
  i386|i686) arch_name="x32" ;;
  *)
    echo "Unsupported architecture for gitleaks installer: ${arch_name}" >&2
    exit 2
    ;;
esac

ARCHIVE="gitleaks_${VERSION#v}_${os_name}_${arch_name}.tar.gz"
CHECKSUMS="gitleaks_${VERSION#v}_checksums.txt"
BASE_URL="https://github.com/gitleaks/gitleaks/releases/download/${VERSION}"

verify_sha256() {
  local checksum_file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$checksum_file"
    return
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$checksum_file"
    return
  fi
  echo "Neither sha256sum nor shasum is available for checksum verification." >&2
  exit 127
}

mkdir -p "${BIN_DIR}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

curl -sSfL "${BASE_URL}/${ARCHIVE}" -o "${tmp_dir}/${ARCHIVE}"
curl -sSfL "${BASE_URL}/${CHECKSUMS}" -o "${tmp_dir}/${CHECKSUMS}"

grep " ${ARCHIVE}\$" "${tmp_dir}/${CHECKSUMS}" > "${tmp_dir}/gitleaks.sha256"
(
  cd "${tmp_dir}"
  verify_sha256 gitleaks.sha256
)

tar -xzf "${tmp_dir}/${ARCHIVE}" -C "${tmp_dir}" gitleaks
install -m 0755 "${tmp_dir}/gitleaks" "${BIN_DIR}/gitleaks"

echo "Installed gitleaks ${VERSION} to ${BIN_DIR}/gitleaks"
