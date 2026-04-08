#!/bin/bash
# upload.sh — Upload MoA build artifacts to Cloudflare R2
#
# Prerequisites:
#   1. Install wrangler: npm install -g wrangler
#   2. Login: wrangler login
#   3. Create R2 bucket: wrangler r2 bucket create moa-downloads
#   4. Set custom domain in Cloudflare dashboard:
#      R2 bucket -> Settings -> Custom Domains -> downloads.mymoa.app
#
# Usage:
#   ./scripts/r2-deploy/upload.sh              # upload all
#   ./scripts/r2-deploy/upload.sh --macos      # macOS only
#   ./scripts/r2-deploy/upload.sh --page       # download page only

set -euo pipefail

BUCKET="moa-downloads"
VERSION="0.1.7"
PROJECT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
TAURI_BUNDLE="$PROJECT_DIR/clients/tauri/src-tauri/target/release/bundle"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

upload() {
  local src="$1"
  local dst="$2"
  if [ -f "$src" ]; then
    echo -e "${GREEN}Uploading${NC} $dst"
    wrangler r2 object put "$BUCKET/$dst" --file "$src"
  else
    echo -e "${YELLOW}Skip${NC} $src (not found)"
  fi
}

upload_page() {
  echo -e "\n${GREEN}=== Download Page ===${NC}"
  upload "$PROJECT_DIR/scripts/r2-deploy/download-page.html" "download/index.html"
  # Upload icon assets
  upload "$PROJECT_DIR/web/dist/MoA_icon_128px.png" "assets/MoA_icon_128px.png"
  upload "$PROJECT_DIR/web/dist/Gemini_Generated_Image_MoA_profile_resized.png" "assets/MoA_profile.png"
}

upload_macos() {
  echo -e "\n${GREEN}=== macOS ===${NC}"
  # DMG
  local dmg=$(find "$TAURI_BUNDLE/dmg" -name "*.dmg" 2>/dev/null | head -1)
  [ -n "$dmg" ] && upload "$dmg" "desktop/macos/MoA-${VERSION}-aarch64.dmg"
  # .app (zip it)
  local app=$(find "$TAURI_BUNDLE/macos" -name "*.app" -maxdepth 1 2>/dev/null | head -1)
  if [ -n "$app" ]; then
    local zipfile="/tmp/MoA-${VERSION}-macos.app.zip"
    ditto -c -k --sequesterRsrc "$app" "$zipfile"
    upload "$zipfile" "desktop/macos/MoA-${VERSION}-macos.app.zip"
    rm -f "$zipfile"
  fi
}

upload_windows() {
  echo -e "\n${GREEN}=== Windows ===${NC}"
  local msi=$(find "$TAURI_BUNDLE/msi" -name "*.msi" 2>/dev/null | head -1)
  [ -n "$msi" ] && upload "$msi" "desktop/windows/MoA-${VERSION}-x64.msi"
  local nsis=$(find "$TAURI_BUNDLE/nsis" -name "*.exe" 2>/dev/null | head -1)
  [ -n "$nsis" ] && upload "$nsis" "desktop/windows/MoA-${VERSION}-x64-setup.exe"
}

upload_linux() {
  echo -e "\n${GREEN}=== Linux ===${NC}"
  local appimage=$(find "$TAURI_BUNDLE/appimage" -name "*.AppImage" 2>/dev/null | head -1)
  [ -n "$appimage" ] && upload "$appimage" "desktop/linux/MoA-${VERSION}-x86_64.AppImage"
  local deb=$(find "$TAURI_BUNDLE/deb" -name "*.deb" 2>/dev/null | head -1)
  [ -n "$deb" ] && upload "$deb" "desktop/linux/MoA-${VERSION}-amd64.deb"
  local rpm=$(find "$TAURI_BUNDLE/rpm" -name "*.rpm" 2>/dev/null | head -1)
  [ -n "$rpm" ] && upload "$rpm" "desktop/linux/MoA-${VERSION}-x86_64.rpm"
}

upload_ios() {
  echo -e "\n${GREEN}=== iOS ===${NC}"
  local ipa=$(find "$TAURI_BUNDLE" -name "*.ipa" 2>/dev/null | head -1)
  [ -n "$ipa" ] && upload "$ipa" "mobile/ios/MoA-${VERSION}.ipa"
}

upload_android() {
  echo -e "\n${GREEN}=== Android ===${NC}"
  local apk=$(find "$PROJECT_DIR/clients/tauri/src-tauri/gen/android" -name "*.apk" 2>/dev/null | head -1)
  [ -n "$apk" ] && upload "$apk" "mobile/android/MoA-${VERSION}.apk"
  local aab=$(find "$PROJECT_DIR/clients/tauri/src-tauri/gen/android" -name "*.aab" 2>/dev/null | head -1)
  [ -n "$aab" ] && upload "$aab" "mobile/android/MoA-${VERSION}.aab"
}

# Parse args
case "${1:-all}" in
  --page)    upload_page ;;
  --macos)   upload_macos ;;
  --windows) upload_windows ;;
  --linux)   upload_linux ;;
  --ios)     upload_ios ;;
  --android) upload_android ;;
  all|*)
    echo "Uploading MoA v${VERSION} to Cloudflare R2 (bucket: ${BUCKET})"
    upload_page
    upload_macos
    upload_windows
    upload_linux
    upload_ios
    upload_android
    echo -e "\n${GREEN}Done!${NC} Files available at https://downloads.mymoa.app/"
    ;;
esac
