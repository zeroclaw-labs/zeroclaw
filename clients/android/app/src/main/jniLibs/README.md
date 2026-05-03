# jniLibs/

Native library mount point for the Android APK. Each subdirectory
holds the `libzeroclaw_android.so` for one ABI:

```
jniLibs/
├── arm64/      # aarch64-linux-android  — most modern phones
├── arm32/      # armv7-linux-androideabi — older 32-bit phones
├── x86_64/     # x86_64-linux-android    — Android x86_64 emulators
└── x86/        # i686-linux-android      — Android x86 emulators
```

## Populating this directory

The `.so` files are NOT committed (gitignored — they're large
build outputs). Generate them with:

```bash
# Single ABI (default arm64, debug)
bash scripts/android/build-ndk.sh

# All four ABIs (release; needed for Play Store)
bash scripts/android/build-ndk.sh --all-abis
```

Prerequisites: the Android NDK installed via Android Studio
(SDK Manager → SDK Tools → "NDK (Side by side)") and either
`ANDROID_NDK_HOME`, `NDK_HOME`, or `ANDROID_HOME` set.

The script does the rust-target install, the cross-compile, and the
copy into the correct ABI directory automatically.

## Closing the audit follow-up

This directory + the build script close E7 from
`docs/audit-2026-05-03.md` (Android NDK cross-compile not wired —
bridge fails at runtime without `libzeroclaw.so`). Once the script
runs successfully on a contributor's machine, the resulting APK
includes the bridge and the Android client can talk to the
embedded ZeroClaw runtime.
