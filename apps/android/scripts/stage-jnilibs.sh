#!/usr/bin/env bash
# Stage the bundled native payload into the APK source tree so the build embeds it.
#
#   zeroclaw  binary  -> app/src/main/jniLibs/arm64-v8a/libzeroclaw.so
#   busybox   binary  -> app/src/main/jniLibs/arm64-v8a/libbusybox.so
#   web-dist  (opt)   -> app/src/main/assets/web-dist/
#
# Why jniLibs/lib*.so: Android 10+ blocks exec() from the app data dir (W^X). The package manager
# extracts native libs to an executable dir, so shipping the binaries AS native libs is the only
# durable way to bundle-and-exec them. They must be named lib*.so or the packager drops them.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(git -C "$ROOT" rev-parse --show-toplevel)"
JNI="$ROOT/app/src/main/jniLibs/arm64-v8a"
ASSETS="$ROOT/app/src/main/assets"

# libzeroclaw.so = the ZeroClaw Rust gateway built for aarch64-linux-android.
# The Android integration is built from this monorepo and includes the `android_*` client tools
# (crates/zeroclaw-tools/src/android/) which dial the private UDS socket. Override ZC_BIN to stage
# a different local build, e.g.:
#   ZC_BIN=/path/to/zeroclaw/target/aarch64-linux-android/release/zeroclaw \
#     apps/android/scripts/stage-jnilibs.sh
# `cargo ndk -t arm64-v8a build --release -p zeroclaw --bin zeroclaw` builds the default path.
ZC_BIN="${ZC_BIN:-$REPO_ROOT/target/aarch64-linux-android/release/zeroclaw}"
BB_BIN="${BB_BIN:-}"
BB_SHA256="${BB_SHA256:-}"
WEB_DIST="${WEB_DIST:-}"

# The agent binary is the one genuinely required payload — without it there is no app.
[ -f "$ZC_BIN" ] || { echo "missing zeroclaw binary: $ZC_BIN"; exit 1; }

mkdir -p "$JNI"
cp "$ZC_BIN" "$JNI/libzeroclaw.so"
# BusyBox is an optional custom-build extra. The stock v0.3 release uses Android's /system/bin;
# typed android_* tools do not depend on BusyBox. SSH is provided in-process by Apache MINA, so
# remove legacy native payloads rather than accidentally inheriting them from an earlier stage.
if [ -n "$BB_BIN" ]; then
  [ -f "$BB_BIN" ] || { echo "missing explicitly requested BusyBox: $BB_BIN" >&2; exit 1; }
  [ -n "$BB_SHA256" ] || {
    echo "BB_SHA256 is required when staging an optional BusyBox payload" >&2
    exit 1
  }
  ACTUAL_BB_SHA256="$(shasum -a 256 "$BB_BIN" | awk '{print $1}')"
  [ "$ACTUAL_BB_SHA256" = "$(printf '%s' "$BB_SHA256" | tr '[:upper:]' '[:lower:]')" ] || {
    echo "optional BusyBox hash does not match BB_SHA256" >&2
    exit 1
  }
  cp "$BB_BIN" "$JNI/libbusybox.so"
else
  rm -f "$JNI/libbusybox.so"
  echo ">> no optional BusyBox — using /system/bin"
fi
rm -f "$JNI/libdropbear.so"
echo ">> staged native libs:"
ls -lh "$JNI"/lib*.so

# Record where the staged ZeroClaw binary actually came from. The APK surfaces this ref and commit,
# so derive them from the binary's checkout instead of accepting an unrelated caller-supplied label.
ZC_SRC_DIR="$(cd "$(dirname "$ZC_BIN")" && pwd)"
ZC_TOP="$(git -C "$ZC_SRC_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$ZC_TOP" ]; then
  # A binary outside a checkout carries no source provenance. Reporting a branch would mislead.
  ZC_REF_DETECTED=unknown
  ZC_COMMIT_DETECTED=unknown
else
  ZC_REF_DETECTED="$(git -C "$ZC_SRC_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
  ZC_COMMIT_DETECTED="$(git -C "$ZC_SRC_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
  git -C "$ZC_SRC_DIR" diff --quiet HEAD 2>/dev/null || ZC_COMMIT_DETECTED="$ZC_COMMIT_DETECTED-dirty"
fi
if [ -z "$WEB_DIST" ]; then
  if [ -n "$ZC_TOP" ] && [ -f "$ZC_TOP/web/dist/index.html" ]; then
    WEB_DIST="$ZC_TOP/web/dist"
  else
    WEB_DIST="$ROOT/artifacts/web-dist"
  fi
fi
mkdir -p "$ROOT/artifacts"
{
  printf '%s\n' '# Written by stage-jnilibs.sh; describes the staged libzeroclaw.so. Do not hand-edit.'
  printf 'ZEROCLAW_REF=%q\n' "$ZC_REF_DETECTED"
  printf 'ZEROCLAW_COMMIT=%q\n' "$ZC_COMMIT_DETECTED"
  printf 'ZEROCLAW_BIN=%q\n' "$ZC_BIN"
} > "$ROOT/artifacts/zeroclaw-provenance.env"
echo ">> staged zeroclaw provenance: ref=$ZC_REF_DETECTED commit=$ZC_COMMIT_DETECTED"

rm -rf "$ASSETS/web-dist"
if [ -d "$WEB_DIST" ]; then
  mkdir -p "$ASSETS/web-dist"
  cp -R "$WEB_DIST/." "$ASSETS/web-dist/"
  echo ">> staged web-dist into assets"
else
  echo ">> no web-dist at $WEB_DIST — skipping (gateway runs without the bundled dashboard)"
fi

# Bundle only the generic Android capability skill. Personal app-automation examples remain in the
# source tree for opt-in custom builds; shipping them in every APK would silently steer a fresh
# install toward acting on real accounts and people.
rm -rf "$ASSETS/skills"
[ -f "$ROOT/skills/android/SKILL.md" ] || {
  echo "missing required Android skill: $ROOT/skills/android/SKILL.md" >&2
  exit 1
}
mkdir -p "$ASSETS/skills/android"
cp "$ROOT/skills/android/SKILL.md" "$ASSETS/skills/android/SKILL.md"
echo ">> staged skill into assets: android"

echo ">> jniLibs staged. Now build the APK (see apps/android/README.md)."
