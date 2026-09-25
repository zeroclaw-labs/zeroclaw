# v0.3 sideload release

This release starts a new permanent Android signing lineage. It does not update the debug-signed
`v0.2.0` prerelease in place; users uninstall that build and install `v0.3.0` cleanly.

This is an experimental sideload release, not a Play Store package. It supports Android 11+ and is
arm64-v8a only because the bundled ZeroClaw executable is built for arm64.

Required trusted local release inputs:

- `ZERODROID_RELEASE_STORE_FILE`
- `ZERODROID_RELEASE_STORE_PASSWORD`
- `ZERODROID_RELEASE_KEY_ALIAS`
- `ZERODROID_RELEASE_KEY_PASSWORD`
- `ZEROCLAW_ANDROID_RELEASE_CERT_SHA256`

The keystore and passwords never belong in git. The expected public certificate fingerprint is
supplied separately so `scripts/build-release.sh` can reject an artifact signed by the wrong key.

The release script requires a clean ZeroClaw checkout and builds both the dashboard and Android
native binary itself. Use Node 24 as declared by the repository's `.nvmrc`.
It validates the source commit, but the repository does not yet expose one canonical Rust
build-toolchain pin for the script to consume. Byte-for-byte native reproducibility is therefore
not claimed.

Build the artifact; this does not publish:

```sh
ANDROID_HOME=/absolute/path/to/Android/sdk \
apps/android/scripts/build-release.sh 0.3.0
```

The output is `dist/zerodroid-v0.3.0.apk`.

Back up the permanent keystore in an encrypted offline location before publishing the first RC.
Losing it means future APKs cannot update installed `v0.3+` builds.

## Verify the artifacts

From `dist/`:

```sh
shasum -a 256 -c SHA256SUMS
"$ANDROID_HOME/build-tools/36.1.0/apksigner" verify --verbose zerodroid-v0.3.0.apk
"$ANDROID_HOME/build-tools/36.1.0/apksigner" verify --print-certs zerodroid-v0.3.0.apk
```

The printed SHA-256 certificate digest must match the separately managed release fingerprint.
Share the checksum and certificate fingerprint alongside the APK through a separate trusted
channel.

## Clean-install test

Uninstalling deletes the old app-private config, provider credential, memories, and sessions. Do
not export or reuse credentials from the old installation.

```sh
adb uninstall org.zerodroid.bridge
adb install apps/android/dist/zerodroid-v0.3.0.apk
adb shell am start -n org.zerodroid.bridge/.MainActivity
```

Then verify on screen:

1. The app opens as **zerodroid**, with Phone tools and Autonomous control both off.
2. Enter a current provider key; do not reuse a credential exposed in logs or chat.
3. Grant only the basic permissions needed for the test.
4. Enable Phone tools, then explicitly enable zerodroid under Android Accessibility.
5. Leave Autonomous control off for inspection-only use. Enabling it must show the high-risk
   disclosure and restart a running gateway before actions become available.
6. Grant Draw over other apps only if testing the CellClaw-style bubble.
7. Start the agent and follow `docs/06-on-device-verification.md`.

Generated installs put all gateway routes under a random per-install path because Android TCP
loopback is shared by every app UID. The overlay also sends a separate `X-Webhook-Secret`. Remote
clients must use the full dashboard URL displayed by zerodroid, then complete normal pairing.

For later v0.3+ builds signed by the same certificate, verify normal upgrade behavior with
`adb install -r` and confirm app data survives.
