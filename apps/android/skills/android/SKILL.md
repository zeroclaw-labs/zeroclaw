---
name: android
description: Use ZeroClaw's Android-native tools and authenticated local bridge to inspect or control the host phone, its apps, Intents, sensors, radios, messages, camera, location, and system settings.
---

# Android device control

You are running **on an Android phone**. Prefer the typed `android_*` tools for UI and supported
device facts. The separately approval-gated `shell` tool reaches platform utilities in
`/system/bin` (`am`, `cmd`, `content`, `pm`, `settings`, `svc`, `dumpsys`, `getprop`).
`/system/bin` for capabilities without a typed tool. Prefer the patterns below over guessing.

## Ground rules

- Always run one command at a time via the shell tool; read its raw output before the next step.
- Some actions need a runtime permission the shell user lacks (sending SMS directly, toggling
  radios, placing a call). When a command is permission-blocked, fall back to an **Intent** that
  opens the app pre-filled for the user to confirm (e.g. `DIAL` instead of `CALL`, `SENDTO`
  instead of direct SMS write). Note which you used.
- `dumpsys <service>` is read-only state. `cmd <service> ...` and `am`/`content` can change state.

## Device + power + sensors (read)

```bash
getprop ro.product.model                 # device model
getprop ro.build.version.release         # Android version
dumpsys battery | grep -iE 'level|temp|status|health'
dumpsys sensorservice | grep -iE 'accelerometer|gyroscope|magnetic|proximity|light'
settings get system screen_brightness
```

## Apps + Intents (launch / act)

```bash
pm list packages | grep -i <name>                                  # find a package
am start -n <pkg>/<activity>                                        # launch an app
am start -a android.intent.action.VIEW -d "https://example.com"    # open a URL
am start -a android.intent.action.VIEW -d "geo:25.79,-80.13?q=coffee"   # open Maps at a point
am start -a android.intent.action.DIAL -d tel:5551234567           # dialer pre-filled (no perm)
am start -a android.intent.action.SENDTO -d sms:5551234567 --es sms_body "hi"   # SMS composer
am start -a android.media.action.IMAGE_CAPTURE                     # open camera to capture
```
Share to a specific app (e.g. WhatsApp) via an explicit-package SEND intent:
```bash
am start -a android.intent.action.SEND -t text/plain --es android.intent.extra.TEXT "msg" <pkg>
```

## Messages + contacts (read via ContentProvider)

```bash
content query --uri content://sms/inbox --projection address:body   # recent SMS (needs READ_SMS)
content query --uri content://com.android.contacts/contacts --projection display_name
content query --uri content://call_log/calls --projection number:date:duration
```

## Notifications

```bash
cmd notification post -S bigtext -t "Title" tag "Body text"          # post a heads-up notification
```

## Connectivity + settings (state, and change where permitted)

```bash
cmd wifi status                          # wifi state
settings get global bluetooth_on         # bt state (1/0)
svc wifi enable                          # toggle radios (may require perms; fall back to a Settings intent)
am start -a android.settings.WIFI_SETTINGS   # open the relevant Settings screen for the user
settings put system screen_brightness 120
```

## Bridge auth (required)

The bridge requires a token on every request (any local app can reach 127.0.0.1, so it must
authenticate). Read it once and pass it on every call:
```bash
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" \
  http://127.0.0.1:8470/sensors
```
`/health` is the only public endpoint. Without a valid token every other endpoint returns 401.
In the v0.3 generated profile, shell and file mutation remain separately approval-gated even when
Autonomous control is on. Prefer the typed `android_*` tools; do not try to bypass a missing
approval channel from the floating bubble.

## Native surface via the zerodroid-bridge APK (BT/BLE, GMS, sensors)

Some capabilities are NOT reachable from the shell — Bluetooth/BLE, precise GMS FusedLocation,
typed sensor reads. The **zerodroid-bridge APK** (`apps/android/`) exposes them over a
loopback HTTP server. Every call below reads the app-private token and sends it in an authorization
header; never issue a bare request or print the token. These shell examples require `curl`; if the
device image does not provide it, use the typed `android_*` tools and report the unavailable extra
surface rather than downloading a binary or bypassing policy:

```bash
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/caps
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/device
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/sensors
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/location
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/wifi/scan
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/ble/scan
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/telephony
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/contacts
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" http://127.0.0.1:8470/sms/list
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/sms/send?to=5551234567&body=hi"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/notify?title=Hi&text=from+agent"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/intent?action=android.intent.action.VIEW&data=geo:25.76,-80.19"
# on-device AI (ML Kit, offline, free):
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/ml/langid?text=Bonjour"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/ml/translate?text=Bonjour&to=en"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/ml/ocr?path=<image_dir-from-caps>/x.jpg"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/ml/entities?text=meet+me+at+5pm"
curl -s -H "Authorization: Bearer $(cat "$HOME/.zeroclaw/bridge-token")" "http://127.0.0.1:8470/ml/barcode?path=<image_dir-from-caps>/qr.jpg"
```
For OCR or barcode input, first read `image_dir` from `/caps` and place the image under that exact
app-owned directory; arbitrary `/sdcard` paths are intentionally rejected. URL-encode query
values. `/sms/send` and `/intent` take ACTION on the device — confirm intent
with the user for anything outbound (sending a message, placing a call, opening an app).
If an authenticated request to `http://127.0.0.1:8470/caps` fails, the bridge may be down — inspect
the HTTP status before concluding that (401 means the token was omitted or stale). Fall back to the shell
surface (`dumpsys`, `cmd wifi`) for what it can reach, and tell the user BLE needs the bridge.

## Seeing and driving the screen (`android_*` tools — prefer these over `input`)

When the `android_*` tools are present, they are the correct way to observe and control the UI of
**any** app. They work untethered on an unrooted phone; the shell's `input tap` does not (it needs
the adb-shell UID).

- `android_launch` — open an app by package (resolves its launcher intent).
- `android_screenshot` — capture the screen; the image goes to the model, so **look before you
  tap**. This is the reliable way to locate a control.
- `android_ui_read` — the accessibility tree: text, content-description, class, clickable flag,
  bounds and center per node, plus the foreground package. Great when the app exposes real labels;
  many apps (especially image-heavy ones) expose almost nothing — fall back to the screenshot.
- `android_action` — `tap` (by `{x,y}` or by `text`), `swipe`, `scroll`, `text` (type into the
  focused field), and `key` (back/home/recents). Every call must pass `expect_package`.
- `android_dialog` — separately approval-gated `allow`/`deny` for a recognized Android system
  dialog. Ordinary actions cannot operate permission or installer windows.

Rules:
- **Screenshot before every tap; never guess a coordinate.** If the screen isn't what you expected,
  screenshot again and reassess rather than tapping blind.
- `android_action` is approval-gated by default — it acts on a real device in real apps. Say what
  you are about to do before doing it, and stop after acting unless told to continue.
- Always pass the package returned by the latest read/capture as `expect_package`; the service
  revalidates it immediately before mutation and refuses focus drift.
- If a call fails with `service_unavailable`, the accessibility service is off: tell the user to
  enable **Settings → Accessibility → zerodroid**. Don't fall back to `input tap`; it won't work.

## Google services

GMS apps (Maps, Drive, Gmail, Gemini) are reachable by **Intent** today (launch + pre-fill).
GMS *Java* APIs (FusedLocation now via the bridge; Maps SDK, Drive, FCM) come through the
in-app bridge. For ad-hoc location, `/location` (bridge) or `dumpsys location`; open Maps via a
`geo:LAT,LNG` URI.

## Reporting

When you change device state, say exactly what you ran and the observed result. When you fall
back to an Intent (because a direct action needed a permission), say so and tell the user what
to confirm on screen.
