#!/usr/bin/env bash
set -euo pipefail

# Reproducible build probe:
# - Build twice from clean state
# - Compare artifact SHA256
# - Emit JSON + markdown artifacts for auditability

PROFILE="${PROFILE:-release}"
BINARY_NAME="${BINARY_NAME:-zeroclaw}"
OUTPUT_DIR="${OUTPUT_DIR:-artifacts}"
FAIL_ON_DRIFT="${FAIL_ON_DRIFT:-false}"
ALLOW_BUILD_ID_DRIFT="${ALLOW_BUILD_ID_DRIFT:-true}"
TARGET_ROOT="${CARGO_TARGET_DIR:-target}"

mkdir -p "${OUTPUT_DIR}"

host_target="$(rustc -vV | sed -n 's/^host: //p')"
artifact_path="${TARGET_ROOT}/${host_target}/${PROFILE}/${BINARY_NAME}"

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file}" | awk '{print $1}'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file}" | awk '{print $1}'
    return 0
  fi
  if command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "${file}" | awk '{print $NF}'
    return 0
  fi
  echo "no SHA256 tool found (need sha256sum, shasum, or openssl)" >&2
  exit 5
}

build_once() {
  local pass="$1"
  cargo clean
  cargo build --profile "${PROFILE}" --locked --target "${host_target}" --verbose
  if [ ! -f "${artifact_path}" ]; then
    echo "expected artifact not found: ${artifact_path}" >&2
    exit 2
  fi
  cp "${artifact_path}" "${OUTPUT_DIR}/repro-build-${pass}.bin"
  sha256_file "${OUTPUT_DIR}/repro-build-${pass}.bin"
}

extract_build_id() {
  local bin="$1"
  if ! command -v readelf >/dev/null 2>&1; then
    echo ""
    return 0
  fi
  readelf -n "${bin}" 2>/dev/null | sed -n 's/^\s*Build ID: //p' | head -n 1
}

is_build_id_only_drift() {
  local first="$1"
  local second="$2"

  if ! command -v objcopy >/dev/null 2>&1; then
    return 1
  fi

  local tmp1 tmp2
  tmp1="$(mktemp)"
  tmp2="$(mktemp)"
  cp "${first}" "${tmp1}"
  cp "${second}" "${tmp2}"
  objcopy --remove-section .note.gnu.build-id "${tmp1}" >/dev/null 2>&1 || true
  objcopy --remove-section .note.gnu.build-id "${tmp2}" >/dev/null 2>&1 || true

  if cmp -s "${tmp1}" "${tmp2}"; then
    rm -f "${tmp1}" "${tmp2}"
    return 0
  fi
  rm -f "${tmp1}" "${tmp2}"
  return 1
}

sha1="$(build_once first)"
sha2="$(build_once second)"

status="match"
drift_reason="none"
first_build_id=""
second_build_id=""
if [ "${sha1}" != "${sha2}" ]; then
  status="drift"
  drift_reason="binary_sha_mismatch"
  first_build_id="$(extract_build_id "${OUTPUT_DIR}/repro-build-first.bin")"
  second_build_id="$(extract_build_id "${OUTPUT_DIR}/repro-build-second.bin")"
  if is_build_id_only_drift "${OUTPUT_DIR}/repro-build-first.bin" "${OUTPUT_DIR}/repro-build-second.bin"; then
    status="drift_build_id_only"
    drift_reason="gnu_build_id_note_diff"
  fi
fi

cat > "${OUTPUT_DIR}/reproducible-build.json" <<EOF
{
  "schema_version": "zeroclaw.audit.v1",
  "event_type": "reproducible_build",
  "profile": "${PROFILE}",
  "target": "${host_target}",
  "binary": "${BINARY_NAME}",
  "first_sha256": "${sha1}",
  "second_sha256": "${sha2}",
  "status": "${status}",
  "drift_reason": "${drift_reason}",
  "allow_build_id_drift": "${ALLOW_BUILD_ID_DRIFT}",
  "first_build_id": "${first_build_id}",
  "second_build_id": "${second_build_id}"
}
EOF

cat > "${OUTPUT_DIR}/reproducible-build.md" <<EOF
# Reproducible Build Check

- Profile: \`${PROFILE}\`
- Target: \`${host_target}\`
- Binary: \`${BINARY_NAME}\`
- First SHA256: \`${sha1}\`
- Second SHA256: \`${sha2}\`
- Result: \`${status}\`
- Drift reason: \`${drift_reason}\`
- First Build ID: \`${first_build_id:-n/a}\`
- Second Build ID: \`${second_build_id:-n/a}\`
- Allow build-id-only drift: \`${ALLOW_BUILD_ID_DRIFT}\`
EOF

if [ "${status}" = "drift" ] && [ "${FAIL_ON_DRIFT}" = "true" ]; then
  exit 3
fi

if [ "${status}" = "drift_build_id_only" ] && [ "${FAIL_ON_DRIFT}" = "true" ] && [ "${ALLOW_BUILD_ID_DRIFT}" != "true" ]; then
  exit 4
fi
