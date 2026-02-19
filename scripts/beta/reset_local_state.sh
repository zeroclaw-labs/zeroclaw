#!/bin/bash
# Reset Local State Script for OpenClaw Studio Beta Testing
# Safely clears localStorage, cache, and temporary files

set -e  # Exit on error

echo "🧹 OpenClaw Studio - Reset Local State"
echo "======================================="
echo ""

# Check if running from project root
if [ ! -f "package.json" ]; then
  echo "❌ Error: Must run from project root (openclaw-studio/)"
  echo "   Current directory: $(pwd)"
  exit 1
fi

echo "⚠️  WARNING: This will clear:"
echo "   - Browser localStorage (modes, settings, pinned models)"
echo "   - Browser cache (requires manual refresh)"
echo "   - Temporary build files"
echo ""
echo "   This will NOT clear:"
echo "   - node_modules"
echo "   - Source code"
echo "   - OpenClaw Gateway data"
echo ""

# Prompt for confirmation
read -p "Continue? (y/N): " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
  echo "❌ Aborted."
  exit 0
fi

echo ""
echo "🗑️  Clearing temporary build files..."
if [ -d "dist" ]; then
  rm -rf dist
  echo "   ✅ Removed dist/"
fi

if [ -d ".vite" ]; then
  rm -rf .vite
  echo "   ✅ Removed .vite/"
fi

if [ -d ".tanstack" ]; then
  rm -rf .tanstack
  echo "   ✅ Removed .tanstack/"
fi

echo ""
echo "🌐 localStorage must be cleared manually in browser:"
echo ""
echo "   Chrome/Edge:"
echo "   1. Open http://localhost:3000"
echo "   2. Press F12 (DevTools)"
echo "   3. Go to 'Application' tab"
echo "   4. Click 'Local Storage' → 'http://localhost:3000'"
echo "   5. Right-click → 'Clear'"
echo ""
echo "   Firefox:"
echo "   1. Open http://localhost:3000"
echo "   2. Press F12 (DevTools)"
echo "   3. Go to 'Storage' tab"
echo "   4. Click 'Local Storage' → 'http://localhost:3000'"
echo "   5. Right-click → 'Delete All'"
echo ""
echo "   Safari:"
echo "   1. Safari → Preferences → Advanced → 'Show Develop menu'"
echo "   2. Develop → Empty Caches"
echo "   3. Develop → Show Web Inspector → Storage tab → Clear"
echo ""

echo "🔄 Recommended next steps:"
echo "   1. Clear localStorage (see instructions above)"
echo "   2. Run: npm run dev"
echo "   3. Open http://localhost:3000"
echo "   4. Hard refresh: Cmd+Shift+R (Mac) or Ctrl+Shift+R (Windows/Linux)"
echo ""
echo "✅ Local state reset complete!"
