# ZeroClaw Android UI control layer

**What it adds:** the agent running *inside* the APK can now **see and drive the screen of any
app on the phone**, untethered, on an unrooted device — read the UI tree, take a screenshot, tap,
swipe, scroll, type, press keys, separately confirm privileged dialogs, and launch apps by package.

> Status legend: ✅ proven on-device · 🔌 wired (needs the right device/setup) · 🛠 designed/next

| Piece | What | Status |
|---|---|---|
| `UiAccessibilityService` | AccessibilityService: tree read, `dispatchGesture`, `takeScreenshot` | ✅ physical arm64 Android 16 device |
| `UiSocketServer` | UDS RPC server at `<filesDir>/ui.sock`, 12 ops, protocol `2` | ✅ |
| Rust `android_*` tools | screenshot, UI read/action, launch, and read-only device facts | ✅ |
| Generic `screenshot` | delegates to the guarded Android capture path on Android | ✅ |
| Screenshot → vision | tool emits `[IMAGE:<path>]`, provider inlines it as a native image block | ✅ (needs a vision-capable model + quota) |
| `OverlayService` bubble | draggable floating bubble → expands to a chat input → `POST /webhook` | ✅ |
| Capture-hide coordination | bubble auto-hides before screenshots/gestures so it never appears in frame | ✅ |

---

## 1. Why an accessibility service (and not the shell)

The shell path (`screencap`, `uiautomator dump`, `input tap`) works **only under the adb-shell UID**
(2000, `shell` SELinux context, in the `input` group) — i.e. only while tethered to a PC. An APK's
own `untrusted_app` UID is denied all three, and this phone has no `su`.

Untethered + unrooted ⇒ **AccessibilityService is the only mechanism**. Three manifest flags in
`res/xml/accessibility_config.xml` are the entire capability surface:

| Flag | Unlocks |
|---|---|
| `canRetrieveWindowContent` | `rootInActiveWindow` → the UI tree (`read`, `tap by text`, `dialog`) |
| `canPerformGestures` | `dispatchGesture` → `tap`, `swipe`, `scroll` |
| `canTakeScreenshot` | `AccessibilityService.takeScreenshot` (API 30+) — **no MediaProjection consent dialog** |

The user must enable the service once in **Settings → Accessibility → zerodroid**. It cannot be
self-granted by the app; while tethered it can be pre-set with
`adb shell settings put secure enabled_accessibility_services …`.

## 2. Transport: a Unix-domain socket, not loopback HTTP

The Rust agent runs as the app UID but **cannot touch the screen** — only the component holding the
accessibility binding can. So the two halves must do IPC.

```
┌──────────────────── Android device (unrooted, one APK) ─────────────────┐
│ libzeroclaw.so (rust agent) ── the brain                                 │
│    android_* tools ──UnixStream──▶ <filesDir>/ui.sock  (0700, app UID)   │
│ UiSocketServer  ──broadcast + ResultReceiver──▶ UiAccessibilityService    │
│                                                  ├ rootInActiveWindow    │
│                                                  ├ dispatchGesture       │
│                                                  └ takeScreenshot        │
└──────────────────────────────────────────────────────────────────────────┘
```

The rest of the Android bridge uses a token-authenticated loopback server on `127.0.0.1:8470`. The UI layer
deliberately does **not**: a loopback TCP port is reachable by *every* app on the device, so a bearer
token is the only thing standing between a malicious app and full screen control. A `LocalSocket` in
the **FILESYSTEM namespace** under `filesDir` is kernel-isolated to the app UID — no other app can
even `open()` it. **The socket is the trust boundary**; no port, no shared secret, no conflict.

(Android's *abstract* LocalSocket namespace would have been the opposite: network-namespace-scoped
and reachable by any app. FILESYSTEM namespace is the one that isolates.)

### Protocol

Newline-delimited JSON, one request per line, one response per line. Request cap 64 KiB.

```jsonc
→ {"id":"1","op":"tap","args":{"expect_package":"com.example.app","x":636,"y":2100}}
← {"id":"1","ok":true,"data":{"dispatched":true}}
← {"id":"1","ok":false,"error":{"code":"service_unavailable","message":"…"}}
```

| op | args | notes |
|---|---|---|
| `ping` | — | returns `{version, service_connected}` — the liveness probe |
| `launch` | `package`, optional `activity` | resolves the launcher intent; `not_found` if none |
| `screenshot` | `max_width` (default 540) | returns a bounded base64 PNG; the Rust client writes the temporary image file |
| `read` | `max_depth` (default 15) | flattened node list: text, desc, class, clickable, bounds, center |
| `foreground` | — | current package and optional activity |
| `foreground` | — | current package + activity |
| `tap` | `expect_package` + `{x,y}` **or** `{text}` | target revalidated at execution time |
| `swipe` | `expect_package`, `x1,y1,x2,y2`, `duration_ms` (300) | |
| `scroll` | `expect_package`, `direction`, optional `x,y` | |
| `text` | `expect_package`, `text` | types into the focused editable |
| `key` | `expect_package`, `key` | `back`, `home`, `recents`, … |
| `device` | `what` | read-only sensors, location, or telephony facts |
| `dialog` | `expect_package`, `button=allow|deny` | privileged path, separate from ordinary actions |

Error codes: `service_unavailable`, `bad_args`, `timeout`, `no_focus`, `not_found`,
`screenshot_failed`, `unsupported_op`, and `internal`. The canonical wire spec lives at
`docs/book/src/tools/android-bridge-protocol.md`.

The a11y service runs in the **default process** (not `:a11y`) so it shares `cacheDir` with the
socket server and can hand a screenshot over as a path rather than a Binder blob.

## 3. Rust tool family

Six tools in `crates/zeroclaw-tools/src/android/`, registered only when **both** hold:

```toml
[android]
enabled = true                     # default false — the whole layer is opt-in
require_approval_for_actions = true # default true (fail-closed)
screenshot_max_width = 540
```

…**and** `is_android()` is true. On any other platform, or with `enabled = false`, the registry
lists no android tools and behavior is byte-identical — that is the "option" guarantee, and it has
a test.

- `android_screenshot` writes the PNG and emits `[IMAGE:<abs-path>]`, which the provider layer
  inlines as a native image block for every model family. No new vision plumbing.
- `android_ui_read` returns a bounded accessibility tree and precomputed tap points.
- `android_device` reads sensors, location, or telephony through ordinary Android APIs and does
  not require the accessibility service.
- `android_action` is the security-sensitive one: **excluded from `default_auto_approve()`** and
  approval-gated through the central `ApprovalManager`, mirroring the browser-automation posture.
  `require_approval_for_actions = true` + no approving operator ⇒ the tool is not registered at all
  (fail-closed, not fail-open).
- `android_dialog` preserves system permission/install confirmation as a separate tool. It is
  omitted whenever the active profile would auto-approve it, including full autonomy and `"*"`.
  Ordinary actions refuse system-dialog packages.

## 4. Floating bubble (`OverlayService`)

A draggable bubble on `TYPE_APPLICATION_OVERLAY` that expands into a chat input, posts the text to
the on-device gateway (`POST /webhook`), and renders the reply — so the phone can be driven without
leaving the app you're looking at. Needs `SYSTEM_ALERT_WINDOW`, granted from the app's
"Floating bubble" switch (OxygenOS grants the appop on toggle; `adb shell appops set` is blocked
there because shell lacks `MANAGE_APP_OPS_MODES`).

Four non-obvious couplings:

1. **Cleartext to loopback.** `POST http://127.0.0.1:<port>/<private-path>/webhook` is blocked by default on
   targetSdk 28+ ("Cleartext HTTP traffic to 127.0.0.1 not permitted"). `res/xml/network_security_config.xml`
   permits cleartext **to localhost only**; everything else stays TLS-required.
2. **Capture-hide.** The bubble is a window like any other, so it would appear in every screenshot
   and could swallow a synthetic tap. `UiAccessibilityService` calls
   `OverlayVisibilityController.requestHide(…)` before capturing or dispatching
   (`HIDE_SCREENSHOT_MS = 800`, `HIDE_GESTURE_MS = 500`, `SETTLE_MS = 150`) and short-circuits when
   the overlay isn't active, so there is no cost when the bubble is off.
3. **Input focus.** The panel must be focusable while the owner types a prompt, but it relinquishes
   that focus immediately after Send. Otherwise Android continues to report the bubble's own
   `EditText` as the focused input, and a later `android_action text` types into the overlay instead
   of into the app underneath. The accessibility service independently refuses to type into a node
   owned by `org.zerodroid.bridge`, so the invariant holds even if another surface forgets to
   release focus.
4. **CellClaw state palette.** Overlay colours live in `res/values/colors.xml`, copied from the
   actual CellClaw overlay palette rather than scattered through Kotlin: green idle, blue
   monitoring, orange thinking/executing/approval, grey paused/offline, and red error. The bubble's
   first frame is derived from `RuntimeState.gatewayState` (not a purple placeholder) and follows
   later supervisor transitions. Purple remains only where CellClaw uses it: as the panel/action
   accent.

Generated installs put every gateway route under a random per-install path because Android shares
TCP loopback across app UIDs. Pair-code minting additionally requires an app-private admin secret;
the path alone grants no authority. Inside that namespace the bubble self-pairs once, persists its
token in Keystore-backed preferences,
(`/admin/paircode/new` → pair → Bearer), then sends an independent `X-Webhook-Secret` on the
prompt. The APK's `bridge-token`, gateway pairing token, private path, and webhook secret are
different values; none is logged.

## 5. Setup playbook (untethered result, tethered once)

1. Build this checkout's `aarch64-linux-android` binary → stage into
   `jniLibs/arm64-v8a/libzeroclaw.so`
   → build + sideload the APK.
2. Open zerodroid → provider + API key → **Start agent**.
3. **Settings → Accessibility → zerodroid → On** (one-time; survives reboots).
4. If the OEM restricts background services, grant the app's battery-optimization exemption.
5. Toggle **Floating bubble** on (grants `SYSTEM_ALERT_WINDOW` via the system prompt).
6. Enable **Phone tools (read-only)** in the app. Enable **Autonomous control** separately only for
   active UI driving. Verify the UI bridge with `ping` →
   `{"service_connected": true}`.
7. Unplug. Everything above runs with no cable, no wifi-debug pairing, no PC.

### Development gotchas

- `adb forward tcp:42617` collides with a desktop ZeroClaw daemon on the same port — forward to a
  different local port (e.g. `tcp:42620`).
- The skill-bundle config key is `skill_bundles` (underscore) **and** the agent must reference the
  bundle: `[agents.phone] skill_bundles = ["default"]`.
- Prefer direct `run-as` + `tee` over nested `sh -c` quoting when writing config over adb.
- Provider HTTP errors are not evidence that screen capture failed. Verify the PNG at the Android
  boundary first, then diagnose provider credentials, model availability, quota, and networking
  independently.

## 6. Threat model delta

This layer is a **step change** in blast radius: an agent that can read and tap any screen can read
every message, drain a wallet app, and act as you inside any authenticated session. Accordingly:

- the whole family is off by default and requires two independent switches (`enabled` + platform);
- `android_action` is never auto-approved by default;
- every ordinary mutation names and revalidates the foreground package, while privileged system
  dialogs use a separate non-auto-approved tool;
- password nodes and zerodroid's own credential UI are excluded from observation;
- screenshots use a bounded private cache with retention cleanup;
- the transport is UID-isolated rather than token-guarded;
- release builds must preserve the default-off capability switches and the explicit Android grants.
