#!/usr/bin/env bash
set -euo pipefail

# This script is intended to be run via 'bazel run //:install'
# It installs the zeroclaw binary to the user's local bin directory.

# Detect the location of the built binary
# Bazel sets the environment variable BUILD_WORKSPACE_DIRECTORY when running via 'bazel run'
BINARY_PATH="zeroclaw"
DEST_DIR="${HOME}/.local/bin"
DEST_PATH="${DEST_DIR}/zeroclaw"

if [[ ! -f "${BINARY_PATH}" ]]; then
    echo "Error: zeroclaw binary not found at ${BINARY_PATH}"
    echo "Make sure you are running this via 'bazel run //:install'"
    exit 1
fi

mkdir -p "${DEST_DIR}"

echo "Installing zeroclaw to ${DEST_PATH}..."
cp -f "${BINARY_PATH}" "${DEST_PATH}"
chmod +x "${DEST_PATH}"

echo "Successfully installed zeroclaw!"
echo "Make sure ${DEST_DIR} is in your PATH."
