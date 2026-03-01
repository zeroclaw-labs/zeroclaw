#!/usr/bin/env bash
set -euo pipefail

if command -v cc >/dev/null 2>&1; then
    echo "C compiler already available: $(command -v cc)"
    cc --version | head -n1 || true
    exit 0
fi

echo "::warning::Missing 'cc' on runner. Attempting to install a C toolchain."

sudo_if_available() {
    if command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        "$@"
    fi
}

if command -v apt-get >/dev/null 2>&1; then
    sudo_if_available apt-get update
    sudo_if_available env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential pkg-config
elif command -v yum >/dev/null 2>&1; then
    sudo_if_available yum install -y gcc gcc-c++ make pkgconfig
elif command -v dnf >/dev/null 2>&1; then
    sudo_if_available dnf install -y gcc gcc-c++ make pkgconf-pkg-config
elif command -v apk >/dev/null 2>&1; then
    sudo_if_available apk add --no-cache build-base pkgconf
else
    echo "::error::No supported package manager found to install 'cc'."
    exit 1
fi

if ! command -v cc >/dev/null 2>&1; then
    echo "::error::Failed to install 'cc'."
    exit 1
fi

echo "Installed C compiler: $(command -v cc)"
cc --version | head -n1 || true
