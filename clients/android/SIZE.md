# ZeroClaw Android - Binary Size Optimization

## Target Sizes

| Component | Target | Notes |
|-----------|--------|-------|
| Native lib (per ABI) | <3MB | Rust, optimized for size |
| APK (arm64-v8a) | <10MB | Single ABI, most users |
| APK (universal) | <20MB | All ABIs, fallback |

## Optimization Strategy

### 1. Rust Native Library

```toml
[profile.release]
opt-level = "z"      # Optimize for size
lto = true           # Link-time optimization
codegen-units = 1    # Better optimization
panic = "abort"      # No unwinding overhead
strip = true         # Remove symbols
```

**Expected savings:** ~40% reduction vs default release

### 2. Android APK

**Enabled:**
- R8 minification (`isMinifyEnabled = true`)
- Resource shrinking (`isShrinkResources = true`)
- ABI splits (users download only their arch)
- Aggressive ProGuard rules

**Removed:**
- `material-icons-extended` (~5MB → 0MB)
- `kotlinx-serialization` (~300KB, unused)
- `ui-tooling-preview` (~100KB, debug only)
- Debug symbols in release

### 3. Dependencies Audit

| Dependency | Size | Keep? |
|------------|------|-------|
| Compose BOM | ~3MB | ✅ Required |
| Material3 | ~1MB | ✅ Required |
| material-icons-extended | ~5MB | ❌ Removed |
| Navigation | ~200KB | ✅ Required |
| DataStore | ~100KB | ✅ Required |
| WorkManager | ~300KB | ✅ Required |
| Security-crypto | ~100KB | ✅ Required |
| Coroutines | ~200KB | ✅ Required |
| Serialization | ~300KB | ❌ Removed (unused) |

### 4. Split APKs

```kotlin
splits {
    abi {
        isEnable = true
        include("arm64-v8a", "armeabi-v7a", "x86_64")
        isUniversalApk = true
    }
}
```

**Result:**
- `app-arm64-v8a-release.apk` → ~10MB (90% of users)
- `app-armeabi-v7a-release.apk` → ~9MB (older devices)
- `app-x86_64-release.apk` → ~10MB (emulators)
- `app-universal-release.apk` → ~18MB (fallback)

## Measuring Size

```bash
# Build release APK
./gradlew assembleRelease

# Check sizes
ls -lh app/build/outputs/apk/release/

# Analyze APK contents
$ANDROID_HOME/build-tools/34.0.0/apkanalyzer apk summary app-release.apk
```

## Future Optimizations

1. **Baseline Profiles** - Pre-compile hot paths
2. **R8 full mode** - More aggressive shrinking
3. **Custom Compose compiler** - Smaller runtime
4. **WebP images** - Smaller than PNG
5. **Dynamic delivery** - On-demand features

## Philosophy

> "Zero overhead. Zero compromise."

Every KB matters. We ship what users need, nothing more.
