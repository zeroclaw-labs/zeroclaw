#!/usr/bin/env bash
# Bounded temporary memory support for hosted Linux ARM64 distribution builds.
set -Eeuo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux ]]; then
  echo "Memory support is restricted to a GitHub Actions Linux runner" >&2
  exit 1
fi
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
dist_swap_root="$(realpath -- "$RUNNER_TEMP")"
dist_swap_file="${ZC_DIST_SWAP_FILE:-}"
dist_swap_identity="${ZC_DIST_SWAP_IDENTITY:-}"

report_resources() {
  free -h
  swapon --show --bytes
  df -h -- "$dist_swap_root"
}

check_owned_file() {
  local name identity
  name="${dist_swap_file##*/}"
  if [[ ! "$name" =~ ^zeroclaw-dist-swap\.[[:alnum:]]{6}$ ]] ||
     [[ "$dist_swap_file" != "$dist_swap_root/$name" ]] ||
     [[ -L "$dist_swap_file" || ! -f "$dist_swap_file" ]]; then
    echo "Refusing cleanup of a nonowned or symlink swap path" >&2
    return 1
  fi
  identity="$(stat -c '%d:%i:%u:%h:%a' -- "$dist_swap_file")" || return 1
  if [[ "$identity" != "$dist_swap_identity" ]] ||
     [[ "$identity" != *":0:1:600" ]]; then
    echo "Refusing cleanup of a replaced, shared or nonprivate swapfile" >&2
    return 1
  fi
}

check_inactive() {
  local active_paths active_path
  active_paths="$(swapon --show=NAME --noheadings --raw)" || return 1
  while IFS= read -r active_path; do
    if [[ "$active_path" == "$dist_swap_file" ]]; then
      echo "Refusing deletion of an active swapfile" >&2
      return 1
    fi
  done <<< "$active_paths"
}

release_owned_file() {
  check_owned_file || return 1
  sudo swapoff -- "$dist_swap_file" || return 1
  check_owned_file || return 1
  check_inactive || return 1
  rm -- "$dist_swap_file"
}

prepare_failure() {
  local status=$?
  trap - EXIT
  if (( status != 0 )) && [[ -n "$dist_swap_file" && -n "$dist_swap_identity" ]]; then
    echo "Memory setup failed; attempting guarded release of its own file" >&2
    if ! release_owned_file; then
      echo "Swapfile retained: guarded release could not be completed" >&2
    fi
  fi
  exit "$status"
}

case "${1:-}" in
  prepare)
    : "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
    dist_swap_file=""
    dist_swap_identity=""
    trap prepare_failure EXIT
    report_resources
    dist_available="$(df --output=avail -B1 -- "$dist_swap_root" | tail -n 1)"
    if (( dist_available < 16 * 1024 * 1024 * 1024 )); then
      echo "Need 8 GiB for swap plus 8 GiB of build headroom" >&2
      exit 1
    fi
    dist_swap_file="$(sudo mktemp "$dist_swap_root/zeroclaw-dist-swap.XXXXXX")"
    sudo chmod 0600 -- "$dist_swap_file"
    dist_swap_identity="$(stat -c '%d:%i:%u:%h:%a' -- "$dist_swap_file")"
    check_owned_file
    sudo fallocate -l 8G -- "$dist_swap_file"
    sudo mkswap -- "$dist_swap_file"
    sudo swapon -- "$dist_swap_file"
    check_owned_file
    dist_active_paths="$(swapon --show=NAME --noheadings --raw)"
    dist_found=false
    while IFS= read -r dist_active_path; do
      if [[ "$dist_active_path" == "$dist_swap_file" ]]; then
        dist_found=true
      fi
    done <<< "$dist_active_paths"
    if [[ "$dist_found" != true ]]; then
      echo "New swapfile was not reported active" >&2
      exit 1
    fi
    report_resources
    # No cleanup receipt is exposed until activation and ownership are verified.
    printf 'swap_file=%s\nswap_identity=%s\n' \
      "$dist_swap_file" "$dist_swap_identity" >> "$GITHUB_OUTPUT"
    ;;
  cleanup)
    dist_cleanup_status=0
    if [[ -z "$dist_swap_file" && -z "$dist_swap_identity" ]]; then
      echo "No successful setup receipt; no swap path will be touched"
    elif [[ -z "$dist_swap_file" || -z "$dist_swap_identity" ]]; then
      echo "Incomplete swap ownership receipt; refusing cleanup" >&2
      dist_cleanup_status=1
    else
      if ! release_owned_file; then
        echo "Guarded release failed; the swapfile is retained" >&2
        dist_cleanup_status=1
      fi
    fi
    report_resources
    if dist_kernel_events="$(sudo dmesg --color=never)"; then
      printf '%s\n' "$dist_kernel_events" | awk '
        /oom-kill|Out of memory|Killed process/ { print; found = 1 }
        END { if (!found) print "No matching OOM event in the readable kernel log" }
      '
    else
      echo "Kernel OOM diagnostics unavailable; cause cannot be inferred"
    fi
    exit "$dist_cleanup_status"
    ;;
  *)
    echo "Usage: dist_runner_memory.sh prepare|cleanup" >&2
    exit 2
    ;;
esac
