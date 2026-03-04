#!/usr/bin/env bash
set -euo pipefail

TARGET="aarch64-linux-android"
RUN_CARGO_CHECK=0
MODE="auto"
DIAGNOSE_LOG=""
JSON_OUTPUT=""
QUIET=0
STRICT=0
ERROR_MESSAGE=""
ERROR_CODE="NONE"
config_linker=""
cargo_linker_override=""
cc_linker_override=""
effective_linker=""

WARNINGS=()
SUGGESTIONS=()
DETECTIONS=()
DETECTION_CODES=()

usage() {
  cat <<'EOF'
Usage:
  scripts/android/termux_source_build_check.sh [--target <triple>] [--mode <auto|termux-native|ndk-cross>] [--run-cargo-check] [--diagnose-log <path>] [--json-output <path|-] [--quiet] [--strict]

Options:
  --target <triple>    Android Rust target (default: aarch64-linux-android)
                       Supported: aarch64-linux-android, armv7-linux-androideabi
  --mode <mode>        Validation mode:
                       auto (default): infer from environment
                       termux-native: expect plain clang + no cross overrides
                       ndk-cross: expect NDK wrapper linker + matching CC_*
  --run-cargo-check    Run cargo check --locked --target <triple> --no-default-features
  --diagnose-log <p>   Diagnose an existing cargo error log and print targeted recovery commands.
  --json-output <p|-]  Write machine-readable report JSON to path, or '-' for stdout.
  --quiet              Suppress informational logs (warnings/errors still emitted).
  --strict             Fail with structured error when any warning is detected.
  -h, --help           Show this help

Purpose:
  Validate Android source-build environment for ZeroClaw, with focus on:
  - Termux native builds using plain clang
  - NDK cross-build overrides (CARGO_TARGET_*_LINKER and CC_*)
  - Common cc-rs linker mismatch failures
EOF
}

log() {
  if [[ "$QUIET" -eq 0 ]]; then
    printf '[android-selfcheck] %s\n' "$*"
  fi
}

warn() {
  printf '[android-selfcheck] warning: %s\n' "$*" >&2
  WARNINGS+=("$*")
}

json_escape() {
  local s="$1"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  s=${s//$'\t'/\\t}
  printf '%s' "$s"
}

json_array_from_args() {
  local first=1
  local item
  printf '['
  for item in "$@"; do
    if [[ "$first" -eq 0 ]]; then
      printf ', '
    fi
    printf '"%s"' "$(json_escape "$item")"
    first=0
  done
  printf ']'
}

json_string_or_null() {
  local s="${1:-}"
  if [[ -z "$s" ]]; then
    printf 'null'
  else
    printf '"%s"' "$(json_escape "$s")"
  fi
}

suggest() {
  log "$*"
  SUGGESTIONS+=("$*")
}

detect_warn() {
  warn "$*"
  DETECTIONS+=("$*")
}

add_detection_code() {
  local code="$1"
  local existing
  for existing in "${DETECTION_CODES[@]}"; do
    if [[ "$existing" == "$code" ]]; then
      return 0
    fi
  done
  DETECTION_CODES+=("$code")
}

emit_json_report() {
  local exit_code="$1"
  [[ -n "$JSON_OUTPUT" ]] || return 0

  local status_text="ok"
  if [[ "$exit_code" -ne 0 ]]; then
    status_text="error"
  fi

  local env_text="non-termux"
  if [[ "${is_termux:-0}" -eq 1 ]]; then
    env_text="termux"
  fi

  local ts
  ts="$(date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || printf '%s' "unknown")"

  local json_payload
  json_payload="$(
    printf '{\n'
    printf '  "schema_version": "zeroclaw.android-selfcheck.v1",\n'
    printf '  "timestamp_utc": "%s",\n' "$(json_escape "$ts")"
    printf '  "status": "%s",\n' "$status_text"
    printf '  "exit_code": %s,\n' "$exit_code"
    printf '  "error_code": "%s",\n' "$(json_escape "$ERROR_CODE")"
    printf '  "error_message": %s,\n' "$(json_string_or_null "$ERROR_MESSAGE")"
    printf '  "target": "%s",\n' "$(json_escape "$TARGET")"
    printf '  "mode_requested": "%s",\n' "$(json_escape "$MODE")"
    printf '  "mode_effective": "%s",\n' "$(json_escape "${effective_mode:-}")"
    printf '  "environment": "%s",\n' "$env_text"
    printf '  "strict_mode": %s,\n' "$([[ "$STRICT" -eq 1 ]] && printf 'true' || printf 'false')"
    printf '  "run_cargo_check": %s,\n' "$([[ "$RUN_CARGO_CHECK" -eq 1 ]] && printf 'true' || printf 'false')"
    printf '  "diagnose_log": %s,\n' "$(json_string_or_null "$DIAGNOSE_LOG")"
    printf '  "config_linker": %s,\n' "$(json_string_or_null "$config_linker")"
    printf '  "cargo_linker_override": %s,\n' "$(json_string_or_null "$cargo_linker_override")"
    printf '  "cc_linker_override": %s,\n' "$(json_string_or_null "$cc_linker_override")"
    printf '  "effective_linker": %s,\n' "$(json_string_or_null "$effective_linker")"
    printf '  "warning_count": %s,\n' "${#WARNINGS[@]}"
    printf '  "detection_count": %s,\n' "${#DETECTIONS[@]}"
    printf '  "warnings": %s,\n' "$(json_array_from_args "${WARNINGS[@]}")"
    printf '  "detections": %s,\n' "$(json_array_from_args "${DETECTIONS[@]}")"
    printf '  "detection_codes": %s,\n' "$(json_array_from_args "${DETECTION_CODES[@]}")"
    printf '  "suggestions": %s\n' "$(json_array_from_args "${SUGGESTIONS[@]}")"
    printf '}\n'
  )"

  if [[ "$JSON_OUTPUT" == "-" ]]; then
    printf '%s' "$json_payload"
    return 0
  fi

  mkdir -p "$(dirname "$JSON_OUTPUT")"
  printf '%s' "$json_payload" >"$JSON_OUTPUT"
}

die() {
  ERROR_MESSAGE="$*"
  printf '[android-selfcheck] error: %s\n' "$*" >&2
  emit_json_report 1
  exit 1
}

enforce_strict_mode() {
  if [[ "$STRICT" -eq 1 && "${#WARNINGS[@]}" -gt 0 ]]; then
    ERROR_CODE="STRICT_WARNINGS"
    die "strict mode failed: ${#WARNINGS[@]} warning(s) detected"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      if [[ $# -lt 2 ]]; then
        ERROR_CODE="BAD_ARGUMENT"
        die "--target requires a value"
      fi
      TARGET="$2"
      shift 2
      ;;
    --run-cargo-check)
      RUN_CARGO_CHECK=1
      shift
      ;;
    --mode)
      if [[ $# -lt 2 ]]; then
        ERROR_CODE="BAD_ARGUMENT"
        die "--mode requires a value"
      fi
      MODE="$2"
      shift 2
      ;;
    --diagnose-log)
      if [[ $# -lt 2 ]]; then
        ERROR_CODE="BAD_ARGUMENT"
        die "--diagnose-log requires a path"
      fi
      DIAGNOSE_LOG="$2"
      shift 2
      ;;
    --json-output)
      if [[ $# -lt 2 ]]; then
        ERROR_CODE="BAD_ARGUMENT"
        die "--json-output requires a path"
      fi
      JSON_OUTPUT="$2"
      if [[ "$JSON_OUTPUT" == "-" ]]; then
        QUIET=1
      fi
      shift 2
      ;;
    --quiet)
      QUIET=1
      shift
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      ERROR_CODE="BAD_ARGUMENT"
      die "unknown argument: $1 (use --help)"
      ;;
  esac
done

case "$TARGET" in
  aarch64-linux-android|armv7-linux-androideabi) ;;
  *)
    ERROR_CODE="BAD_ARGUMENT"
    die "unsupported target '$TARGET' (expected aarch64-linux-android or armv7-linux-androideabi)"
    ;;
esac

case "$MODE" in
  auto|termux-native|ndk-cross) ;;
  *)
    ERROR_CODE="BAD_ARGUMENT"
    die "unsupported mode '$MODE' (expected auto, termux-native, or ndk-cross)"
    ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." >/dev/null 2>&1 && pwd || pwd)"
CONFIG_FILE="$REPO_ROOT/.cargo/config.toml"
cd "$REPO_ROOT"

TARGET_UPPER="$(printf '%s' "$TARGET" | tr '[:lower:]-' '[:upper:]_')"
TARGET_UNDERSCORE="${TARGET//-/_}"
CARGO_LINKER_VAR="CARGO_TARGET_${TARGET_UPPER}_LINKER"
CC_LINKER_VAR="CC_${TARGET_UNDERSCORE}"

is_termux=0
if [[ -n "${TERMUX_VERSION:-}" ]] || [[ "${PREFIX:-}" == *"/com.termux/files/usr"* ]]; then
  is_termux=1
fi

effective_mode="$MODE"
if [[ "$effective_mode" == "auto" ]]; then
  if [[ "$is_termux" -eq 1 ]]; then
    effective_mode="termux-native"
  else
    effective_mode="ndk-cross"
  fi
fi
OFFLINE_DIAGNOSE=0
if [[ -n "$DIAGNOSE_LOG" ]]; then
  OFFLINE_DIAGNOSE=1
fi

extract_linker_from_config() {
  [[ -f "$CONFIG_FILE" ]] || return 0
  awk -v target="$TARGET" '
    $0 ~ "^\\[target\\." target "\\]$" { in_section=1; next }
    in_section && $0 ~ "^\\[" { in_section=0 }
    in_section && $1 == "linker" {
      gsub(/"/, "", $3);
      print $3;
      exit
    }
  ' "$CONFIG_FILE"
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

is_executable_tool() {
  local tool="$1"
  if [[ "$tool" == */* ]]; then
    [[ -x "$tool" ]]
  else
    command_exists "$tool"
  fi
}

ndk_wrapper_for_target() {
  case "$TARGET" in
    aarch64-linux-android) printf '%s\n' "aarch64-linux-android21-clang" ;;
    armv7-linux-androideabi) printf '%s\n' "armv7a-linux-androideabi21-clang" ;;
    *) printf '%s\n' "" ;;
  esac
}

diagnose_cargo_failure() {
  local log_file="$1"
  local ndk_wrapper
  ndk_wrapper="$(ndk_wrapper_for_target)"

  log "cargo check failed; analyzing common Android toolchain issues..."

  if grep -Eq 'failed to find tool "aarch64-linux-android-clang"|failed to find tool "armv7a-linux-androideabi-clang"|ToolNotFound' "$log_file"; then
    detect_warn "detected cc-rs compiler lookup failure for Android target"
    add_detection_code "CC_RS_TOOL_NOT_FOUND"
    if [[ "$effective_mode" == "termux-native" ]]; then
      suggest "suggested recovery (termux-native):"
      suggest "  unset $CARGO_LINKER_VAR"
      suggest "  unset $CC_LINKER_VAR"
      suggest "  pkg install -y clang pkg-config"
      suggest "  command -v clang"
    else
      suggest "suggested recovery (ndk-cross):"
      suggest "  export NDK_TOOLCHAIN=\"\$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin\""
      suggest "  export $CARGO_LINKER_VAR=\"\$NDK_TOOLCHAIN/$ndk_wrapper\""
      suggest "  export $CC_LINKER_VAR=\"\$NDK_TOOLCHAIN/$ndk_wrapper\""
      suggest "  command -v \"\$NDK_TOOLCHAIN/$ndk_wrapper\""
    fi
  fi

  if grep -Eq 'linker `clang` not found|linker .* not found|cannot find linker|failed to find tool "clang"' "$log_file"; then
    detect_warn "detected linker resolution failure"
    add_detection_code "LINKER_RESOLUTION_FAILURE"
    if [[ "$effective_mode" == "termux-native" ]]; then
      suggest "suggested recovery (termux-native):"
      suggest "  pkg install -y clang pkg-config"
      suggest "  command -v clang"
    else
      suggest "suggested recovery (ndk-cross):"
      suggest "  export $CARGO_LINKER_VAR=\"\$NDK_TOOLCHAIN/$ndk_wrapper\""
      suggest "  export $CC_LINKER_VAR=\"\$NDK_TOOLCHAIN/$ndk_wrapper\""
    fi
  fi

  if grep -Eq "target '$TARGET' not found|can't find crate for std|did you mean to run rustup target add" "$log_file"; then
    detect_warn "detected missing Rust target stdlib"
    add_detection_code "MISSING_RUST_TARGET_STDLIB"
    suggest "suggested recovery:"
    suggest "  rustup target add $TARGET"
  fi

  if grep -Eq 'No such file or directory \(os error 2\)' "$log_file"; then
    detect_warn "detected missing binary/file in build chain; verify linker and CC_* variables point to real executables"
    add_detection_code "MISSING_BINARY_OR_FILE"
  fi
}

log "repo: $REPO_ROOT"
log "target: $TARGET"
if [[ "$is_termux" -eq 1 ]]; then
  log "environment: Termux detected"
else
  log "environment: non-Termux (likely desktop/CI)"
fi
log "mode: $effective_mode"

if [[ -z "$DIAGNOSE_LOG" ]]; then
  if ! command_exists rustup; then
    ERROR_CODE="MISSING_RUSTUP"
    die "rustup is not installed"
  fi
  if ! command_exists cargo; then
    ERROR_CODE="MISSING_CARGO"
    die "cargo is not installed"
  fi

  if ! rustup target list --installed | grep -Fx "$TARGET" >/dev/null 2>&1; then
    ERROR_CODE="MISSING_RUST_TARGET"
    die "Rust target '$TARGET' is not installed. Run: rustup target add $TARGET"
  fi
fi

config_linker="$(extract_linker_from_config || true)"
cargo_linker_override="${!CARGO_LINKER_VAR:-}"
cc_linker_override="${!CC_LINKER_VAR:-}"

if [[ -n "$config_linker" ]]; then
  log "config linker ($TARGET): $config_linker"
else
  warn "no linker configured for $TARGET in .cargo/config.toml"
fi

if [[ -n "$cargo_linker_override" ]]; then
  log "env override $CARGO_LINKER_VAR=$cargo_linker_override"
fi
if [[ -n "$cc_linker_override" ]]; then
  log "env override $CC_LINKER_VAR=$cc_linker_override"
fi

effective_linker="${cargo_linker_override:-${config_linker:-clang}}"
log "effective linker: $effective_linker"

if [[ "$OFFLINE_DIAGNOSE" -eq 0 ]]; then
  if [[ "$effective_mode" == "termux-native" ]]; then
    if ! command_exists clang; then
      if [[ "$is_termux" -eq 1 ]]; then
        ERROR_CODE="TERMUX_CLANG_MISSING"
        die "clang is required in Termux. Run: pkg install -y clang pkg-config"
      fi
      warn "clang is not available on this non-Termux host; termux-native checks are partial"
    fi

    if [[ "${config_linker:-}" != "clang" ]]; then
      warn "Termux native build should use linker = \"clang\" for $TARGET"
    fi

    if [[ -n "$cargo_linker_override" && "$cargo_linker_override" != "clang" ]]; then
      warn "Termux native build usually should unset $CARGO_LINKER_VAR (currently '$cargo_linker_override')"
    fi
    if [[ -n "$cc_linker_override" && "$cc_linker_override" != "clang" ]]; then
      warn "Termux native build usually should unset $CC_LINKER_VAR (currently '$cc_linker_override')"
    fi

    suggest "suggested fixups (termux-native):"
    suggest "  unset $CARGO_LINKER_VAR"
    suggest "  unset $CC_LINKER_VAR"
    suggest "  command -v clang"
  else
    if [[ -n "$cargo_linker_override" && -z "$cc_linker_override" ]]; then
      warn "cross-build may still fail in cc-rs crates; consider setting $CC_LINKER_VAR=$cargo_linker_override"
    fi

    if [[ -n "$cargo_linker_override" ]]; then
      suggest "suggested fixup (ndk-cross):"
      suggest "  export $CC_LINKER_VAR=\"$cargo_linker_override\""
    else
      warn "NDK cross mode expects $CARGO_LINKER_VAR to point to an NDK clang wrapper"
      suggest "suggested fixup template (ndk-cross):"
      suggest "  export NDK_TOOLCHAIN=\"\$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin\""
      if [[ "$TARGET" == "aarch64-linux-android" ]]; then
        suggest "  export $CARGO_LINKER_VAR=\"\$NDK_TOOLCHAIN/aarch64-linux-android21-clang\""
        suggest "  export $CC_LINKER_VAR=\"\$NDK_TOOLCHAIN/aarch64-linux-android21-clang\""
      else
        suggest "  export $CARGO_LINKER_VAR=\"\$NDK_TOOLCHAIN/armv7a-linux-androideabi21-clang\""
        suggest "  export $CC_LINKER_VAR=\"\$NDK_TOOLCHAIN/armv7a-linux-androideabi21-clang\""
      fi
    fi
  fi

  if ! is_executable_tool "$effective_linker"; then
    if [[ "$effective_mode" == "termux-native" ]]; then
      if [[ "$is_termux" -eq 1 ]]; then
        ERROR_CODE="LINKER_NOT_EXECUTABLE"
        die "effective linker '$effective_linker' is not executable in PATH"
      fi
      warn "effective linker '$effective_linker' not executable on this non-Termux host"
    else
      warn "effective linker '$effective_linker' not found (expected for some desktop hosts without NDK toolchain)"
    fi
  fi
fi

if [[ -n "$DIAGNOSE_LOG" ]]; then
  if [[ ! -f "$DIAGNOSE_LOG" ]]; then
    ERROR_CODE="MISSING_DIAGNOSE_LOG"
    die "diagnose log file does not exist: $DIAGNOSE_LOG"
  fi
  log "diagnosing provided cargo log: $DIAGNOSE_LOG"
  diagnose_cargo_failure "$DIAGNOSE_LOG"
  log "diagnosis completed"
  enforce_strict_mode
  emit_json_report 0
  exit 0
fi

if [[ "$RUN_CARGO_CHECK" -eq 1 ]]; then
  tmp_log="$(mktemp -t zeroclaw-android-check-XXXXXX.log)"
  cleanup_tmp_log() {
    rm -f "$tmp_log"
  }
  trap cleanup_tmp_log EXIT

  log "running cargo check --locked --target $TARGET --no-default-features"
  set +e
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/zeroclaw-android-selfcheck-target}" \
    cargo check --locked --target "$TARGET" --no-default-features 2>&1 | tee "$tmp_log"
  cargo_status="${PIPESTATUS[0]}"
  set -e

  if [[ "$cargo_status" -ne 0 ]]; then
    diagnose_cargo_failure "$tmp_log"
    ERROR_CODE="CARGO_CHECK_FAILED"
    die "cargo check failed (exit $cargo_status)"
  fi

  log "cargo check completed successfully"
else
  log "skip cargo check (use --run-cargo-check to enable)"
fi

log "self-check completed"
enforce_strict_mode
emit_json_report 0
