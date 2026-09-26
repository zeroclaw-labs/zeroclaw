#!/usr/bin/env bash
# Input is git diff --name-only output. Only this native process probe is gated;
# Windows compilation remains unconditional and catches wider crate breakage.
set -euo pipefail

event="${1:-}"
paths="${2:-}"
if [[ "$event" != pull_request || ! -s "$paths" ]]; then
    echo 'windows_recovery=true'
    exit 0
fi

# Include the TaskRecord owner and module wiring, not just authority.rs. Cargo
# and toolchain changes can alter sysinfo or the Windows API dependency.
pattern='^crates/zeroclaw-runtime/src/(control_plane/|lib\.rs$)|(^|/)Cargo\.(toml|lock)$|(^|/)build\.rs$|(^|/)rust-toolchain(\.toml)?$|^\.cargo/|^\.github/actions/|^\.github/workflows/ci\.yml$|^scripts/ci/windows_recovery_change_filter(\.test)?\.sh$'
if grep -Eq "$pattern" "$paths"; then
    echo 'windows_recovery=true'
else
    status=$?
    if [[ "$status" == 1 ]]; then
        echo 'windows_recovery=false'
    else
        # Unreadable change evidence must not silently remove coverage.
        echo 'windows_recovery=true'
    fi
fi
