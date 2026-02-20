# Turn Your Old Android Phone Into a Personal Assistant

MobileClaw turns an old Android phone into an on-device control center for your daily actions.

MobileClaw is a lightweight autonomous AI agent with UI designed to run on Android devices:
- a fast Rust-first core under the mobile UX layer
- a small-footprint runtime model suitable for older/low-cost devices
- modular architecture where providers/channels/tools/memory are swappable
- broad provider and messaging ecosystem compatibility
- secure-by-default design principles for tool and data handling

MobileClaw uses ZeroClaw as its runtime foundation, adapted for Android-native assistant workflows.
ZeroClaw is a lightweight, secure autonomous AI agent infrastructure designed as a high-performance alternative to OpenClaw.
It is written in Rust and designed to run with a very low resource footprint.
ZeroClaw upstream project: https://github.com/zeroclaw-labs/zeroclaw

## What MobileClaw Core Inherits from ZeroClaw

- Performance: ZeroClaw reported cold start under 10ms, with binary size around 3.4MB
- Low footprint: ZeroClaw reported runtime memory usage under 5MB in minimal setups
- Architecture: trait-based subsystem with swappable providers, channels, tools, and memory via configuration
- Interoperability: support for 22+ providers and messaging APIs (including OpenAI, Anthropic, OpenRouter, Telegram, Discord, Slack)
- Security: preemptive protocol focus to reduce leak risk before incidents occur

Note: the numbers above describe the ZeroClaw core/runtime characteristics. 
Full MobileClaw app behavior on a phone depends on Android, enabled capabilities, permissions, and active integrations.


## Watch the Demo

[![MobileClaw demo video](https://img.youtube.com/vi/-3fpcQAL6II/maxresdefault.jpg)](https://youtu.be/-3fpcQAL6II)

[![MobileClaw demo video 2](https://img.youtube.com/vi/HtNMcQIDsZ8/maxresdefault.jpg)](https://youtu.be/HtNMcQIDsZ8)

[![MobileClaw demo video 3](https://img.youtube.com/vi/OGHg3Fzgg70/maxresdefault.jpg)](https://youtu.be/OGHg3Fzgg70)


## What You Can Do Today

With the right permissions and toggles enabled, MobileClaw Agent can:

- read and browse files from device storage
- read latest SMS and call log
- start call flow and SMS flow from chat
- get current location (if location fix is available)
- read contacts and calendar events
- post and inspect active notifications
- access device/network/sensor status
- open apps, open URLs, and open system settings
- launch camera/audio/document workflows
- expose Bluetooth/NFC related actions
- provide hardware/device information and memory stats

It also supports optional advanced flows:

- Accessibility-based UI automation controls (tap/swipe/click/back/home/recents)
- in-app browser session tools (open/navigate/read page state/fetch page text)

## Important Reality Check

MobileClaw is powerful, but Android security still applies:

- many features require runtime permissions
- some capabilities require OS-level switches (for example Accessibility service)
- some actions open system UI/apps where Android expects user confirmation

Anyway, use it with caution!!

## Build and Run on a Real Android Device

### 1) Prepare your phone

1. Enable **Developer Options** on the phone.
2. Enable **USB debugging**.
3. Connect phone by USB and accept the debug prompt.

Check device connection:

```bash
adb devices
```

You should see your device as `device`.

### 2) Build and launch

From this repository:

```bash
cd mobile-app
npm install
npm run android
```

If Metro cannot be reached through USB, run:

```bash
adb reverse tcp:8081 tcp:8081
```

### 3) First-time setup in app

1. Open the **Device** tab.
2. Enable the capabilities you want.
3. Accept Android permission prompts.
4. If using UI automation, enable Accessibility service from the in-app instructions.


## Install APK from GitHub Releases

### 1) Download

1. Open this repository's **Releases** page on GitHub.
2. Open the latest stable release.
3. Download the `mobileclaw-<version>.apk` asset.
4. (Optional) download `SHA256SUMS` and verify checksum before install.

### 2) Install on Android

1. Open the downloaded APK on your phone.
2. If prompted, allow install from this source (browser/files app).
3. Continue and finish installation.

### 3) First launch setup

1. Open **MobileClaw**.
2. Go to the **Device** tab.
3. Enable the capabilities you need and grant requested permissions.
4. If using UI automation, enable Accessibility service from in-app guidance.


## Notes

- This README focuses on user outcomes and running the app quickly.
- For deeper engineering internals, see docs and module sources in the repo.
