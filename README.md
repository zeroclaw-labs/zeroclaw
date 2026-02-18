# Turn Your Old Android Phone Into a Personal Assistant

MobileClaw turns an old Android phone into an on-device control center for your daily actions.

## Watch the Demo

[![MobileClaw demo video](https://img.youtube.com/vi/-3fpcQAL6II/maxresdefault.jpg)](https://youtu.be/-3fpcQAL6II)

Demo video: https://youtu.be/-3fpcQAL6II

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

## Project Layout

- `mobile-app/` - Android app (React Native + Expo)
- `src/` - ZeroClaw runtime/core modules used across the project

ZeroClaw upstream project: https://github.com/zeroclaw-labs/zeroclaw

## Notes

- This README focuses on user outcomes and running the app quickly.
- For deeper engineering internals, see docs and module sources in the repo.
