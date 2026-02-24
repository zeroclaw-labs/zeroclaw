#!/usr/bin/env bash
set -euo pipefail

# Install pinned gitleaks binary into a writable bin directory.
# Usage: ./scripts/ci/install_gitleaks.sh <bin_dir> [version]

BIN_DIR="${1:-${RUNNER_TEMP:-/tmp}/bin}"
VERSION="${2:-${GITLEAKS_VERSION:-v8.24.2}}"
ARCHIVE="gitleaks_${VERSION#v}_linux_x64.tar.gz"
CHECKSUMS="gitleaks_${VERSION#v}_checksums.txt"
BASE_URL="https://github.com/gitleaks/gitleaks/releases/download/${VERSION}"

mkdir -p "${BIN_DIR}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

curl -sSfL "${BASE_URL}/${ARCHIVE}" -o "${tmp_dir}/${ARCHIVE}"
curl -sSfL "${BASE_URL}/${CHECKSUMS}" -o "${tmp_dir}/${CHECKSUMS}"

grep " ${ARCHIVE}\$" "${tmp_dir}/${CHECKSUMS}" > "${tmp_dir}/gitleaks.sha256"
(
  cd "${tmp_dir}"
  sha256sum -c gitleaks.sha256
)

tar -xzf "${tmp_dir}/${ARCHIVE}" -C "${tmp_dir}" gitleaks
install -m 0755 "${tmp_dir}/gitleaks" "${BIN_DIR}/gitleaks"

echo "Installed gitleaks ${VERSION} to ${BIN_DIR}/gitleaks"
