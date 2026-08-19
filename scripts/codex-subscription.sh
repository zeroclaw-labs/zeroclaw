#!/usr/bin/env bash
set -euo pipefail

provider=openai-codex
zeroclaw_bin=${ZEROCLAW_BIN:-zeroclaw}
primary_profile=${CODEX_PRIMARY_PROFILE:-default}
alternate_profile=${CODEX_ALT_PROFILE:-alt}

usage() {
  cat <<EOF
Usage: codex-subscription.sh <command>

Commands:
  status             Show all auth profiles and the active profile
  current            Print the active OpenAI Codex profile name
  use primary        Activate $provider:$primary_profile
  use alt            Activate $provider:$alternate_profile
  use <profile>      Activate an explicitly named profile
  toggle             Switch between the configured primary and alt profiles

Environment:
  ZEROCLAW_BIN              ZeroClaw executable (default: zeroclaw)
  CODEX_PRIMARY_PROFILE     Primary profile name (default: default)
  CODEX_ALT_PROFILE         Alternate profile name (default: alt)

The active OpenAI Codex profile is global: every provider configured with
requires_openai_auth = true uses it. This command does not restart ZeroClaw.
EOF
}

auth_list() {
  "$zeroclaw_bin" auth list
}

current_profile() {
  local profile
  profile=$(auth_list | awk -v prefix="$provider:" \
    '$1 == "*" && index($2, prefix) == 1 {sub(prefix, "", $2); print $2; exit}')
  if [[ -z "$profile" ]]; then
    echo 'No active OpenAI Codex profile.' >&2
    return 1
  fi
  printf '%s\n' "$profile"
}

resolve_profile() {
  case "${1:-}" in
    primary) printf '%s\n' "$primary_profile" ;;
    alt|alternate) printf '%s\n' "$alternate_profile" ;;
    '')
      echo 'A profile name is required.' >&2
      return 2
      ;;
    *) printf '%s\n' "$1" ;;
  esac
}

assert_profile_exists() {
  local target=$1
  if ! auth_list | awk -v expected="$provider:$target" \
    '$1 == "*" {profile = $2} $1 != "*" {profile = $1} profile == expected {found = 1} END {exit !found}'; then
    echo "Profile $provider:$target does not exist." >&2
    return 1
  fi
}

assert_profile_unexpired() {
  local target=$1 status_line
  status_line=$("$zeroclaw_bin" auth status | awk -v expected="$provider:$target" \
    '$1 == "*" {profile = $2} $1 != "*" {profile = $1} profile == expected {print; exit}')
  if [[ -z "$status_line" ]]; then
    echo "Profile $provider:$target has no status entry." >&2
    return 1
  fi
  if [[ "$status_line" == *'expired at'* ]]; then
    echo "Profile $provider:$target is expired; sign in before switching." >&2
    return 1
  fi
}

activate() {
  local target=$1 active
  assert_profile_exists "$target"
  assert_profile_unexpired "$target"
  "$zeroclaw_bin" auth use --model-provider "$provider" --profile "$target"
  active=$(current_profile)
  if [[ "$active" != "$target" ]]; then
    echo "Activation verification failed: expected $target, found $active" >&2
    return 1
  fi
  echo "Active Codex subscription: $active"
  echo 'Scope: all Codex-backed agents (global auth profile); no restart performed.'
}

case "${1:-}" in
  status)
    "$zeroclaw_bin" auth status
    ;;
  current)
    current_profile
    ;;
  use)
    activate "$(resolve_profile "${2:-}")"
    ;;
  toggle)
    active=$(current_profile)
    case "$active" in
      "$primary_profile") activate "$alternate_profile" ;;
      "$alternate_profile") activate "$primary_profile" ;;
      *)
        echo "Active profile $active is neither configured toggle endpoint." >&2
        echo "Primary: $primary_profile; alt: $alternate_profile" >&2
        exit 1
        ;;
    esac
    ;;
  -h|--help|help|'')
    usage
    ;;
  *)
    echo "Unknown command: $1" >&2
    usage >&2
    exit 2
    ;;
esac
