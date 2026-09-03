#!/usr/bin/env bash
# Build the signed Android APK for sideload distribution.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(git -C "$ROOT" rev-parse --show-toplevel)"
ANDROID_DIR="$ROOT"
DECLARED_VERSION="$(awk -F= '$1 == "ZEROCLAW_ANDROID_VERSION" { print $2; exit }' "$ROOT/gradle.properties")"
VERSION="$DECLARED_VERSION"

usage() {
  cat <<'USAGE'
Usage: apps/android/scripts/build-release.sh [VERSION]

Builds one signed zerodroid sideload APK.
USAGE
}

case "$#" in
  0) ;;
  1)
    case "$1" in
      -h|--help) usage; exit 0 ;;
      --*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
      *) VERSION="$1" ;;
    esac
    ;;
  *) echo "too many arguments" >&2; usage >&2; exit 2 ;;
esac

case "$VERSION" in
  *[!0-9A-Za-z._-]*|'') echo "invalid version: $VERSION" >&2; exit 2 ;;
esac
[ -n "$DECLARED_VERSION" ] && [ "$VERSION" = "$DECLARED_VERSION" ] || {
  echo "requested version $VERSION does not match gradle.properties ($DECLARED_VERSION)" >&2
  exit 2
}

: "${ZERODROID_RELEASE_STORE_FILE:?set ZERODROID_RELEASE_STORE_FILE}"
: "${ZERODROID_RELEASE_STORE_PASSWORD:?set ZERODROID_RELEASE_STORE_PASSWORD}"
: "${ZERODROID_RELEASE_KEY_ALIAS:?set ZERODROID_RELEASE_KEY_ALIAS}"
: "${ZERODROID_RELEASE_KEY_PASSWORD:?set ZERODROID_RELEASE_KEY_PASSWORD}"
: "${ZEROCLAW_ANDROID_RELEASE_CERT_SHA256:?set ZEROCLAW_ANDROID_RELEASE_CERT_SHA256}"
: "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"

[ -f "$ZERODROID_RELEASE_STORE_FILE" ] || {
  echo "missing release keystore: $ZERODROID_RELEASE_STORE_FILE" >&2
  exit 1
}
if [ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=all)" ]; then
  echo "refusing release from a dirty ZeroClaw checkout" >&2
  exit 1
fi
PINNED_ZEROCLAW_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"

(
  cd "$REPO_ROOT"
  cargo web build
  # Cargo currently normalizes an equivalent lockfile package qualifier while xtask runs.
  git restore Cargo.lock
  git diff --quiet HEAD || {
    echo "dashboard generation changed tracked source files" >&2
    exit 1
  }
)

export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/28.2.13676358}"
[ -d "$ANDROID_NDK_HOME" ] || { echo "missing pinned Android NDK: $ANDROID_NDK_HOME" >&2; exit 1; }
(
  cd "$REPO_ROOT"
  cargo ndk -t arm64-v8a build --release --locked -p zeroclaw --bin zeroclaw
)
ZC_BIN="$REPO_ROOT/target/aarch64-linux-android/release/zeroclaw"
[ -f "$ZC_BIN" ] || { echo "fresh native build missing: $ZC_BIN" >&2; exit 1; }

case "$(uname -s):$(uname -m)" in
  Darwin:*) NDK_HOST="darwin-x86_64" ;;
  Linux:x86_64) NDK_HOST="linux-x86_64" ;;
  Linux:aarch64|Linux:arm64) NDK_HOST="linux-aarch64" ;;
  *) echo "unsupported NDK host: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
READELF="${READELF:-$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$NDK_HOST/bin/llvm-readelf}"
[ -x "$READELF" ] || { echo "llvm-readelf not found under $ANDROID_HOME/ndk" >&2; exit 1; }
ELF_ALIGNMENTS="$("$READELF" -lW "$ZC_BIN" | awk '$1 == "LOAD" {print $NF}' | sort -u)"
[ -n "$ELF_ALIGNMENTS" ] || { echo "native binary has no ELF LOAD segments" >&2; exit 1; }
for alignment in $ELF_ALIGNMENTS; do
  case "$alignment" in
    0x4000|0x10000) ;;
    *) echo "native LOAD segment is not 16/64 KiB aligned: $alignment" >&2; exit 1 ;;
  esac
done

env ZC_BIN="$ZC_BIN" bash "$ROOT/scripts/stage-jnilibs.sh"
[ -f "$ANDROID_DIR/app/src/main/assets/web-dist/index.html" ] || {
  echo "missing staged dashboard; run 'cargo web build' in the pinned ZeroClaw checkout" >&2
  exit 1
}

# shellcheck disable=SC1091
. "$ROOT/artifacts/zeroclaw-provenance.env"
case "$ZEROCLAW_COMMIT" in
  unknown|*-dirty)
    echo "refusing release with untraceable native provenance: $ZEROCLAW_COMMIT" >&2
    exit 1
    ;;
esac
[ "$ZEROCLAW_COMMIT" = "$PINNED_ZEROCLAW_COMMIT" ] || {
  echo "native commit $ZEROCLAW_COMMIT does not match release pin $PINNED_ZEROCLAW_COMMIT" >&2
  exit 1
}

GRADLE_TASKS=(
  :app:testReleaseUnitTest
  :app:lintRelease
  :app:assembleRelease
)
GRADLE_PROPERTIES=(
  "-PZEROCLAW_REF=$ZEROCLAW_REF"
  "-PZEROCLAW_COMMIT=$ZEROCLAW_COMMIT"
)
(
  cd "$ANDROID_DIR"
  ./gradlew --no-daemon \
    :app:clean \
    "${GRADLE_TASKS[@]}" \
    "${GRADLE_PROPERTIES[@]}" \
    --console=plain
)

APK="$ANDROID_DIR/app/build/outputs/apk/release/app-release.apk"
[ -f "$APK" ] || { echo "signed APK missing: $APK" >&2; exit 1; }

ANDROID_BUILD_TOOLS_VERSION="${ANDROID_BUILD_TOOLS_VERSION:-36.1.0}"
APKSIGNER="${APKSIGNER:-$ANDROID_HOME/build-tools/$ANDROID_BUILD_TOOLS_VERSION/apksigner}"
[ -x "$APKSIGNER" ] || { echo "apksigner not found under $ANDROID_HOME" >&2; exit 1; }
ZIPALIGN="${ZIPALIGN:-$(dirname "$APKSIGNER")/zipalign}"
[ -x "$ZIPALIGN" ] || { echo "zipalign not found next to $APKSIGNER" >&2; exit 1; }

EXPECTED_CERT="$(printf '%s' "$ZEROCLAW_ANDROID_RELEASE_CERT_SHA256" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
[ "${#EXPECTED_CERT}" -eq 64 ] || {
  echo "ZEROCLAW_ANDROID_RELEASE_CERT_SHA256 must be a 64-character SHA-256 digest" >&2
  exit 1
}

VERIFY_TMP="$(mktemp -d "${TMPDIR:-/tmp}/zerodroid-elf-verify.XXXXXX")"
trap 'rm -rf "$VERIFY_TMP"' EXIT

APKS=("$APK")
for apk in "${APKS[@]}"; do
  "$ZIPALIGN" -c -P 16 4 "$apk" >/dev/null
  "$APKSIGNER" verify --verbose "$apk" >/dev/null
  ACTUAL_CERT="$($APKSIGNER verify --print-certs "$apk" | awk -F': ' '/certificate SHA-256 digest/{print $2; exit}')"
  ACTUAL_CERT="$(printf '%s' "$ACTUAL_CERT" | tr '[:upper:]' '[:lower:]')"
  [ "$ACTUAL_CERT" = "$EXPECTED_CERT" ] || {
    echo "unexpected signing certificate for $apk" >&2
    exit 1
  }
  EMBEDDED_HASH="$(unzip -p "$apk" lib/arm64-v8a/libzeroclaw.so | shasum -a 256 | awk '{print $1}')"
  SOURCE_HASH="$(shasum -a 256 "$ZC_BIN" | awk '{print $1}')"
  [ "$EMBEDDED_HASH" = "$SOURCE_HASH" ] || {
    echo "embedded native binary hash mismatch in $apk" >&2
    exit 1
  }
  UNEXPECTED_ABIS="$(unzip -Z1 "$apk" | awk '$0 ~ "^lib/[^/]+/" && $0 !~ "^lib/arm64-v8a/" { print }')"
  [ -z "$UNEXPECTED_ABIS" ] || {
    echo "unexpected non-arm64 native payload in $apk:" >&2
    printf '%s\n' "$UNEXPECTED_ABIS" >&2
    exit 1
  }
  while IFS= read -r library; do
    extracted="$VERIFY_TMP/$(basename "$library")"
    unzip -p "$apk" "$library" > "$extracted"
    LIB_ALIGNMENTS="$("$READELF" -lW "$extracted" | awk '$1 == "LOAD" {print $NF}' | sort -u)"
    [ -n "$LIB_ALIGNMENTS" ] || {
      echo "native library has no ELF LOAD segments: $library" >&2
      exit 1
    }
    for alignment in $LIB_ALIGNMENTS; do
      case "$alignment" in
        0x4000|0x10000) ;;
        *) echo "$library is not 16/64 KiB ELF-aligned: $alignment" >&2; exit 1 ;;
      esac
    done
  done < <(unzip -Z1 "$apk" | awk '$0 ~ "^lib/arm64-v8a/.*[.]so$" { print }')
  SKILLS="$(unzip -Z1 "$apk" | awk '$0 ~ "^assets/skills/[^/]+/SKILL[.]md$" { print }')"
  [ "$SKILLS" = "assets/skills/android/SKILL.md" ] || {
    echo "unexpected bundled skill set in $apk:" >&2
    printf '%s\n' "$SKILLS" >&2
    exit 1
  }
  [ "$(unzip -Z1 "$apk" assets/web-dist/index.html)" = "assets/web-dist/index.html" ] || {
    echo "bundled web dashboard missing from $apk" >&2
    exit 1
  }
  if unzip -Z1 "$apk" lib/arm64-v8a/libdropbear.so >/dev/null 2>&1; then
    echo "obsolete Dropbear payload present in $apk" >&2
    exit 1
  fi
done

mkdir -p "$ROOT/dist"
OUT="$ROOT/dist/zerodroid-v$VERSION.apk"
rm -f "$OUT"
cp "$APK" "$OUT"
cp "$ROOT/release/README.md" "$ROOT/dist/INSTALL.md"
cp "$ROOT/release/RELEASE_NOTES.md" "$ROOT/dist/RELEASE_NOTES.md"
printf '%s\n' "$EXPECTED_CERT" > "$ROOT/dist/SIGNING_CERT_SHA256"
cp "$REPO_ROOT/LICENSE-APACHE" "$ROOT/dist/LICENSE-APACHE"
cp "$REPO_ROOT/LICENSE-MIT" "$ROOT/dist/LICENSE-MIT"
{
  cat "$REPO_ROOT/NOTICE"
  printf '\nAndroid application attribution\n===============================\n\n'
  cat "$ROOT/NOTICE"
} > "$ROOT/dist/NOTICE"
(
  cd "$ROOT/dist"
  shasum -a 256 "$(basename "$OUT")" > SHA256SUMS
)

echo ">> signed sideload artifacts"
ls -lh \
  "$OUT" \
  "$ROOT/dist/SHA256SUMS" \
  "$ROOT/dist/INSTALL.md" \
  "$ROOT/dist/RELEASE_NOTES.md" \
  "$ROOT/dist/SIGNING_CERT_SHA256" \
  "$ROOT/dist/LICENSE-APACHE" \
  "$ROOT/dist/LICENSE-MIT" \
  "$ROOT/dist/NOTICE"
cat "$ROOT/dist/SHA256SUMS"
