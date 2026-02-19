# Reset Local State Script for OpenClaw Studio Beta Testing (Windows)
# Safely clears localStorage, cache, and temporary files

Write-Host "🧹 OpenClaw Studio - Reset Local State" -ForegroundColor Cyan
Write-Host "=======================================" -ForegroundColor Cyan
Write-Host ""

# Check if running from project root
if (-Not (Test-Path "package.json")) {
  Write-Host "❌ Error: Must run from project root (openclaw-studio/)" -ForegroundColor Red
  Write-Host "   Current directory: $(Get-Location)" -ForegroundColor Red
  exit 1
}

Write-Host "⚠️  WARNING: This will clear:" -ForegroundColor Yellow
Write-Host "   - Browser localStorage (modes, settings, pinned models)"
Write-Host "   - Browser cache (requires manual refresh)"
Write-Host "   - Temporary build files"
Write-Host ""
Write-Host "   This will NOT clear:"
Write-Host "   - node_modules"
Write-Host "   - Source code"
Write-Host "   - OpenClaw Gateway data"
Write-Host ""

# Prompt for confirmation
$confirmation = Read-Host "Continue? (y/N)"
if ($confirmation -notmatch '^[Yy]$') {
  Write-Host "❌ Aborted." -ForegroundColor Red
  exit 0
}

Write-Host ""
Write-Host "🗑️  Clearing temporary build files..." -ForegroundColor Yellow

if (Test-Path "dist") {
  Remove-Item -Recurse -Force "dist"
  Write-Host "   ✅ Removed dist/" -ForegroundColor Green
}

if (Test-Path ".vite") {
  Remove-Item -Recurse -Force ".vite"
  Write-Host "   ✅ Removed .vite/" -ForegroundColor Green
}

if (Test-Path ".tanstack") {
  Remove-Item -Recurse -Force ".tanstack"
  Write-Host "   ✅ Removed .tanstack/" -ForegroundColor Green
}

Write-Host ""
Write-Host "🌐 localStorage must be cleared manually in browser:" -ForegroundColor Cyan
Write-Host ""
Write-Host "   Chrome/Edge:"
Write-Host "   1. Open http://localhost:3000"
Write-Host "   2. Press F12 (DevTools)"
Write-Host "   3. Go to 'Application' tab"
Write-Host "   4. Click 'Local Storage' → 'http://localhost:3000'"
Write-Host "   5. Right-click → 'Clear'"
Write-Host ""
Write-Host "   Firefox:"
Write-Host "   1. Open http://localhost:3000"
Write-Host "   2. Press F12 (DevTools)"
Write-Host "   3. Go to 'Storage' tab"
Write-Host "   4. Click 'Local Storage' → 'http://localhost:3000'"
Write-Host "   5. Right-click → 'Delete All'"
Write-Host ""

Write-Host "🔄 Recommended next steps:" -ForegroundColor Cyan
Write-Host "   1. Clear localStorage (see instructions above)"
Write-Host "   2. Run: npm run dev"
Write-Host "   3. Open http://localhost:3000"
Write-Host "   4. Hard refresh: Ctrl+Shift+R"
Write-Host ""
Write-Host "✅ Local state reset complete!" -ForegroundColor Green
