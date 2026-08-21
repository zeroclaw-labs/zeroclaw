# zerodroid v0.3.0 release candidate

v0.3.0 is an experimental arm64 Android sideload release that combines the ZeroClaw gateway with the
CellClaw-derived Accessibility and floating-overlay layer in one APK.

## What is included

- Accessibility UI read, screenshot/vision handoff, tap, swipe, scroll, text, keys, app launch,
  system-dialog handling, and read-only device facts over an app-private Unix socket.
- The CellClaw overlay state palette and capture-hide behavior, with the initial colour derived from
  the live gateway state.
- A full Android 12+ flavor and a lite Android 11+ flavor.
- Provider and model selection with 19 hosted defaults validated against models.dev.
- A persistent v0.3 signing certificate, reproducible native-source pin, checksums, and signed
  artifact verification.
- Android-compatible encrypted master-key publication, so gateway pairing persists under the app
  SELinux sandbox without weakening atomic no-replace key creation.

## Safe first-run posture

- Phone tools, autonomous UI control, LAN access, SSH, boot start, and the floating bubble default
  off.
- Read-only mode cannot access the generated provider credential or mutate the phone.
- Autonomous control pre-approves only the typed screen/action tools; shell and file mutation remain
  separately approval-gated.
- Generated gateways use normal pairing inside a random per-install path. Overlay prompts add a
  separate webhook secret because Android TCP loopback is shared across app UIDs.
- Only the generic Android capability skill ships. User-specific app automations are not bundled.

## Upgrade note

The v0.2.0 prerelease was signed with a machine-specific debug key. v0.3.0 starts a permanent
release-signing lineage, so v0.2.0 must be uninstalled before installing v0.3.0. That removes the
old app-private config, provider credential, memories, and sessions. Do not export or reuse a
credential that has appeared in logs or chat.

Future v0.3+ APKs signed by the same certificate can update in place. Release tooling verifies its
fingerprint against `ZEROCLAW_ANDROID_RELEASE_CERT_SHA256`.

## Known limitations

- This is not a Play Store build; the Accessibility-driven autonomous-control surface is intended
  for sideloading on devices the operator owns and controls.
- Enabling Accessibility, Draw over other apps, and OEM battery exemptions remains a manual Android
  settings step after a clean install.
- The floating bubble is disabled in manual-config mode because the app cannot safely infer an
  operator-authored gateway path and webhook secret.
- The stock APK uses Android's `/system/bin`. BusyBox is an optional custom-build payload, not part
  of the v0.3 release; typed Android tools do not depend on it.
- Provider tests require a credential supplied by the tester. The APK contains no provider key.
- The lite flavor compiles and lints at API 30, but still needs a physical Android 11 device or
  emulator sign-off; the physical Android 16 test device validates the full flavor.
- No public GitHub release, tag, Play listing, or update feed is created by the build workflow.
