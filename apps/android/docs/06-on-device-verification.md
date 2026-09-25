# zerodroid on-device verification checklist

Use this after every APK rebuild that changes the bundled ZeroClaw binary, bridge RPC, overlay,
permissions, provider setup, or skills. Run on a physical device first; an emulator cannot prove
OEM accessibility, overlay, battery, telephony, sensor, or app-integration behavior.

## Evidence rules

- Never treat the model's prose or a rendered `<tool_result>` block as evidence. This setup has
  produced convincing but invented match lists, screenshot metadata, sensors, and carrier values.
- Verify tool registration from `GET /api/tools?agent=phone`.
- Verify a screenshot by checking the exact file exists and has nonzero size.
- Verify a tap/type by inspecting the target app afterward. For anything consequential, capture the
  before and after screens independently.
- Verify device facts independently through the bridge API when possible. Do not write exact
  coordinates, phone numbers, identifiers, API keys, bearer tokens, or private message content into
  logs, screenshots, issues, or commits.
- A provider without configured credentials is **not tested**. Record `SKIP — not configured`, not
  pass or fail. Never reuse or expose one provider's credential while testing another.

## 1. Install and preflight

- [ ] For the v0.3.0 signing-line reset, uninstall the debug-signed build and install the signed
      APK cleanly. Do not export the old provider credential before uninstalling.
- [ ] `dumpsys package org.zerodroid.bridge` reports version name `0.3.0`, version code `3`, and
      the expected build provenance.
- [ ] `apksigner verify --print-certs` reports the expected release fingerprint managed outside
      the repository.
- [ ] Fresh app data contains no inherited provider config, memories, sessions, or user-specific
      skill bundle.
- [ ] Phone tools, Autonomous control, encrypted remote access, SSH, boot start, and the bubble are all off.
- [ ] Accessibility, Draw over other apps, and battery exemptions require deliberate re-granting.
- [ ] After the clean-install baseline, a same-certificate `adb install -r` preserves app data and
      Android grants. Test the API 30 floor separately on an Android 11 device or emulator when
      available.
- [ ] Starting the agent creates a live `libzeroclaw.so` child and a fresh `<filesDir>/ui.sock`.
- [ ] `GET <private-path>/health` succeeds through loopback and reports the
      new PID; the unprefixed admin pairing route is not reachable.
- [ ] Turning Autonomous control off while running changes the gateway PID and removes mutating
      Android tools from the effective session; the UI must never show OFF over a stale autonomous
      profile.

## 2. Floating overlay

CellClaw palette is the contract; purple is an accent, never a bubble state.

- [ ] With the gateway stopped, enabling the bubble shows **grey** (`#9E9E9E`) immediately — no
      purple first frame.
- [ ] Starting/restarting the gateway changes the Z bubble to **orange** (`#FF9800`).
- [ ] A running, idle gateway changes it to **green** (`#4CAF50`).
- [ ] Submitting a task changes it to **orange** while the turn is in flight.
- [ ] A failed turn changes it to **red** (`#B00020`); the next submit clears the error state.
- [ ] Stopping the gateway changes it back to **grey**.
- [ ] Blue (`#2196F3`) is reserved for CellClaw-compatible monitoring/reactive mode. Record `N/A`
      until the app emits that state; do not synthesize blue from an unrelated condition.
- [ ] During screenshots and gestures, the overlay stays out of the captured image and never
      intercepts the synthetic input.
- [ ] A multi-step screenshot/gesture turn does not strobe the overlay between tool calls.
- [ ] Type a prompt into the bubble asking the agent to focus a harmless target field (Settings
      search) and enter `zebrafish`. Confirm `zebrafish` appears in Settings and the bubble input is
      empty afterward.
- [ ] Touching the bubble input after a turn reacquires focus and opens the keyboard normally.

## 3. Required tool registry

Query the gateway; do not ask the model which tools it has.

- [ ] `android_action`
- [ ] `android_device`
- [ ] `android_dialog` is present only under a profile that will prompt; it is absent under a
      full-autonomy or wildcard auto-approval profile.
- [ ] `android_launch`
- [ ] `android_screenshot`
- [ ] `android_ui_read`
- [ ] Generic `screenshot` is present and delegates to the bridge on Android.
- [ ] No duplicate/dead Android screenshot path is offered.

## 4. UI-control contract

Run every mutating action with the required `expect_package`.

- [ ] `android_launch` opens Settings and reports `com.android.settings`.
- [ ] `android_ui_read` returns the same foreground package plus real visible nodes/bounds.
- [ ] `android_screenshot` and generic `screenshot` each create a real PNG under the app-owned
      runtime directory; width obeys `max_width` and neither upscales.
- [ ] Deliberately use `expect_package=com.example.target` while Settings is foreground: capture and action
      both refuse and name the actual foreground package.
- [ ] Capture a deliberately blank/mid-render frame if reproducible: bridge returns
      `screenshot_failed: blank frame`, never an empty image.
- [ ] Coordinate tap changes a harmless Settings control.
- [ ] Tap-by-visible-text clicks the intended node or returns `not_found`.
- [ ] Swipe and scroll visibly move a scrollable view; scrolling at the end does not claim success
      unless the gesture is accepted.
- [ ] Text entry succeeds through `ACTION_SET_TEXT` or clipboard `ACTION_PASTE`, reports its method,
      appears in the target field, and restores the owner's prior clipboard.
- [ ] Back, Home, Recents, and Enter each execute or return an honest error.
- [ ] Ordinary `android_action` calls refuse recognized system-dialog packages.
- [ ] `android_dialog button=allow|deny` handles a harmless named decision only after operator
      approval; it rejects arbitrary labels and the wrong foreground package.
- [ ] A password node returns structural metadata with `password=true` but no text or description.
- [ ] With zerodroid itself foreground, UI read and screenshot return `sensitive_target`.

## 5. Read-only device APIs

- [ ] `android_device what=sensors` returns the physical phone's sensor count and recognizable
      vendors; independently compare count/names through the bridge. No emulator `Goldfish` data.
- [ ] `android_device what=telephony` matches the SIM/carrier shown by Android. Confirm no IMEI,
      phone number, or other identifier is exposed by this schema.
- [ ] `android_device what=location` returns `lat/lon/accuracy_m/provider` when permission is granted.
      Confirm only presence/provider/accuracy in the report; do not persist coordinates.
- [ ] Revoke location temporarily: the tool returns a failure (`ACCESS_FINE_LOCATION not granted`),
      not a successful data result. Restore the permission afterward.
- [ ] Unsupported `what=contacts` is rejected with the closed supported list.

## 6. Representative app matrix

The goal is not “every installed app forever”; it is proving the UI layer across distinct Android
UI technologies. Keep all tests reversible and do not send/post/purchase/call.

| App / surface | Why it matters | Safe verification |
|---|---|---|
| Settings | platform Views + system search | launch, read, screenshot, search text, back |
| OEM notes app, if installed | OEM custom UI | launch, create no note; read/screenshot/back only |
| Chromium browser | browser/WebView | open a local/about page, read, scroll, address-field draft |
| Maps, if installed | complex Google UI | launch, read, screenshot, search draft; do not start navigation |
| Camera | SurfaceView/permission boundary | launch and detect UI; do not capture media |
| Messages | RecyclerView + IME | open composer and type a draft; do not send |
| Tester-selected custom-rendered app | sparse accessibility tree | read/screenshot/scroll only; no external action |
| System permission dialog | privileged system surface | detect and report; only press a named harmless button |

For each row:

- [ ] Launch reports the expected package.
- [ ] Two successive captures are readable (retry explicit `blank frame`; never guess).
- [ ] UI-tree result is either useful or honestly sparse; vision covers the sparse case.
- [ ] One reversible tap/scroll/type lands in the target app.
- [ ] Wrong-package guard is deliberately tested once.

## 7. Provider matrix

### Credential-free checks (run for every build)

- [ ] Provider picker populates from `zeroclaw providers` rather than only the fallback list.
- [ ] Every picker entry can be selected without crashing, and local providers ask for a URI rather
      than an API key.
- [ ] Every curated default model prefill is nonblank and present in the current public model
      catalog where that provider publishes one. Providers without a curated default remain blank
      for explicit operator selection rather than receiving a guessed model ID.
- [ ] `scripts/check-model-defaults.py` passes against a freshly downloaded models.dev catalog.
- [ ] A stored model on the explicit retired-model list self-heals to the current default.
- [ ] “Test provider key” preserves the exact provider/model selected and shows the real failure
      line in a dialog (401 credential, 403 caller identity, 404 model, 429 quota, timeout/network).
- [ ] No credential value appears in logs, screenshots, dialogs, test reports, shell history, or
      this checklist.

### Credential-required checks (one row per configured provider)

Record provider, model, date, and result locally. Never copy the credential into the record.

| Capability | Required result |
|---|---|
| Minimal agent turn | exact `READY` response |
| Native tool call | invokes `android_device` and reports an independently verified value |
| Vision (if supported) | describes a known harmless screenshot accurately |
| Multi-step turn | completes beyond 120 seconds without a client timeout or duplicate action |
| 401/403/404/429 error | app displays the provider's real error and targeted hint |
| Fallback model | primary failure advances to a configured fallback and reports that transition |

Providers without credentials remain `SKIP — not configured`. “All providers pass” is never a
valid summary unless every row was actually configured and exercised.

## 8. Channels and remote driving

- [ ] Bubble prompt returns a reply with the gateway bound to loopback only.
- [ ] Two headerless `/webhook` one-shot prompts receive different request scopes and cannot recall
      each other's conversation state. Reusing an explicit `X-Session-Id` preserves context only
      for that named conversation.
- [ ] Pairing-enabled bubble self-pairs and retries once after token rejection.
- [ ] Restarting the bubble reuses its Keystore-backed pairing token rather than accumulating a new
      persistent paired device.
- [ ] A bubble webhook without `X-Webhook-Secret` is rejected even with a valid paired token.
- [ ] A request from loopback with the complete private path but without
      `X-Loopback-Admin-Secret` cannot read or mint a pairing code.
- [ ] The gateway remains bound to `127.0.0.1` while encrypted remote access is enabled.
- [ ] Pubkey SSH allows a local forward to `127.0.0.1:42617`, but rejects forwarding to port 8470,
      other hosts, remote listeners, agent forwarding, and X11.
- [ ] Same-Wi-Fi browser/dashboard can drive the phone only through that encrypted SSH tunnel.
- [ ] USB forwarding is a development console only; unplugging USB does not stop the agent,
      accessibility bridge, bubble, or outbound provider traffic.

## Sign-off record

```text
APK / Android app commit:
Embedded ZeroClaw commit:
Device / Android build:
Android APK: PASS / FAIL
Configured providers actually tested:
Representative apps completed:
Known failures / skipped checks:
Evidence locations (no secrets/PII):
Tester / date:
```
