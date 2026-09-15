#!/usr/bin/env bash

# Classifies changed paths for the Windows leg of the Quality Gate build matrix.
#
# Reads one changed path per line on stdin (`git diff --no-renames --name-only`)
# and prints "true" when any path can change what that leg compiles: the
# Windows `cargo check`, the Windows voice-wake check, and the Windows
# task-owner recovery test. Linux jobs never compile the Windows side of
# platform-conditional code, so the leg runs for:
#   - Cargo manifests, the lockfile, build scripts, the toolchain file, and
#     Cargo configuration, because a dependency or build change can break one
#     target alone;
#   - Rust sources carrying platform-conditional code: cfg predicates on
#     windows or unix, target_os or target_family, std::os::unix or
#     std::os::windows, and the libc, nix, winapi, windows-sys, or winreg
#     crates;
#   - Rust sources the change deletes, whose content the checkout no longer has;
#   - Windows-named paths, Windows resource and manifest files, and PowerShell
#     scripts;
#   - the control-plane module behind the recovery test and the voice-wake
#     module the leg checks directly;
#   - this workflow, its local actions, and this filter.
# When BASE_SHA is set, a Rust source also selects the leg if its base version
# carried platform-conditional code, so deleting the last Windows branch from a
# file still runs the check. A marker read that fails selects the leg.
#
# Known blind spot: a platform-neutral edit can still break an untouched
# Windows-only caller, such as renaming a function whose only Windows call site
# lives in an unchanged file. Push and merge_group runs always check Windows, so
# that break surfaces on the next master run instead of on the pull request.
#
# Prints "false" otherwise. Always exits 0; the workflow step forwards the
# printed value to GITHUB_OUTPUT.

set -euo pipefail

marker_pattern='target_os|target_family|std::os::(unix|windows)|(^|[^A-Za-z0-9_])(libc|nix|winapi|windows_sys|winreg)::|cfg(!|_attr)?\((.*[^A-Za-z0-9_])?(unix|windows)([^A-Za-z0-9_]|$)|^[[:space:]]*(not\()?(unix|windows)\)*,?[[:space:]]*$'

# Succeeds when the source carries platform-conditional code, is gone from the
# checkout, or cannot be read.
has_platform_markers() {
    local path="$1"
    local status

    if [ ! -f "$path" ]; then
        return 0
    fi
    grep -Eq -- "$marker_pattern" "$path" && return 0
    status=$?
    if [ "$status" -ne 1 ]; then
        return 0
    fi

    if [ -n "${BASE_SHA:-}" ]; then
        git --literal-pathspecs grep -q -E -e "$marker_pattern" "$BASE_SHA" -- "$path" 2>/dev/null && return 0
        status=$?
        if [ "$status" -ne 1 ]; then
            return 0
        fi
    fi
    return 1
}

run=false

# Read all of stdin even after a match: stopping early would break the pipe
# feeding this script under `pipefail`.
while IFS= read -r path; do
    case "$path" in
        '')
            ;;
        .github/workflows/ci.yml|\
        .github/actions/*|\
        scripts/ci/windows_check_change_filter*.sh|\
        Cargo.toml|*/Cargo.toml|Cargo.lock|*/Cargo.lock|\
        build.rs|*/build.rs|\
        rust-toolchain|rust-toolchain.toml|.cargo/*|\
        crates/zeroclaw-runtime/src/control_plane/*|\
        crates/zeroclaw-channels/src/voice_wake.rs|\
        windows/*|*/windows/*|*/windows.rs|\
        *.ps1|*.rc|*.manifest)
            run=true
            ;;
        *.rs)
            if [ "$run" = false ] && has_platform_markers "$path"; then
                run=true
            fi
            ;;
    esac
done

echo "$run"
