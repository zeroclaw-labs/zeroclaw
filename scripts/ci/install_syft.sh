#!/usr/bin/env bash
set -euo pipefail

# Install a pinned syft binary into a writable bin directory.
# Usage: ./scripts/ci/install_syft.sh <bin_dir> [version]

BIN_DIR="${1:-${RUNNER_TEMP:-/tmp}/bin}"
VERSION="${2:-${SYFT_VERSION:-v1.42.1}}"

os_name="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$os_name" in
  linux|darwin) ;;
  *)
    echo "Unsupported OS for syft installer: ${os_name}" >&2
    exit 2
    ;;
esac

arch_name="$(uname -m)"
case "$arch_name" in
  x86_64|amd64) arch_name="amd64" ;;
  aarch64|arm64) arch_name="arm64" ;;
  armv7l) arch_name="armv7" ;;
  *)
    echo "Unsupported architecture for syft installer: ${arch_name}" >&2
    exit 2
    ;;
esac

ARCHIVE="syft_${VERSION#v}_${os_name}_${arch_name}.tar.gz"
CHECKSUMS="syft_${VERSION#v}_checksums.txt"
BASE_URL="https://github.com/anchore/syft/releases/download/${VERSION}"

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

awk -v target="${ARCHIVE}" '$2 == target {print $1 "  " $2}' "${tmp_dir}/${CHECKSUMS}" > "${tmp_dir}/syft.sha256"
if [ ! -s "${tmp_dir}/syft.sha256" ]; then
  echo "Missing checksum entry for ${ARCHIVE} in ${CHECKSUMS}" >&2
  exit 1
fi
(
  cd "${tmp_dir}"
  verify_sha256 syft.sha256
)

tar -xzf "${tmp_dir}/${ARCHIVE}" -C "${tmp_dir}" syft
install -m 0755 "${tmp_dir}/syft" "${BIN_DIR}/syft"

echo "Installed syft ${VERSION} to ${BIN_DIR}/syft"
