# Android UI Bridge: UDS JSON-RPC contract (v1)

Shared interface between:
- **Client** = Rust `android_*` tools (`crates/zeroclaw-tools/src/android/`).
- **Server** = Kotlin bridge in the Android APK (`apps/android/`), which brokers to the
  in-APK `AccessibilityService`.

This file is the single source of truth for the wire format. Both sides implement it
verbatim. A protocol change must update the Rust client, Kotlin server, and their tests
together.

## Transport

- **Unix domain socket**, FILESYSTEM namespace (a real path, NOT abstract namespace).
- Path: the APK's private files dir + `/ui.sock`, i.e. `<context.filesDir>/ui.sock`
  → concretely `/data/data/<pkg>/files/ui.sock` (pkg is the bridge APK's applicationId).
- The Rust client learns the path from config `android.socket_path`
  (default `/data/data/org.zerodroid.bridge/files/ui.sock`; overridable).
- Server binds the socket with **0700** perms on the containing dir and unlinks any
  stale socket on startup. Kernel UID-isolation is the ONLY trust boundary; there is
  **no token, no auth field**. Do not add one; the socket path being inside the app's
  private dir is what makes it safe (no other app UID can open it).
- Server accepts multiple concurrent connections; each connection handles one request
  then may be closed by the client. Server MUST NOT assume connection reuse.

## Framing

- **Newline-delimited JSON.** One request = one JSON object on one line terminated by
  `\n`. One response = one JSON object on one line terminated by `\n`.
- UTF-8. No embedded newlines inside the JSON (compact form).
- Max request line: 64 KiB. Max response line: 4 MiB (screenshots are the large case;
  see caps below).

## Request

```json
{ "op": "<string>", "id": "<string, optional echo token>", "args": { ... } }
```

## Response

Success:
```json
{ "ok": true, "id": "<echoed if provided>", "data": { ... } }
```

Failure:
```json
{ "ok": false, "id": "<echoed>", "error": { "code": "<string>", "message": "<human string>" } }
```

Error codes (closed set): `service_unavailable` (accessibility service not connected /
not enabled), `bad_args`, `timeout`, `no_focus` (no input-focused field for `text`),
`not_found` (target text/element/package not found), `screenshot_failed`,
`unsupported_op`, `internal`.

Text entry is the one operation with a documented fallback. Plenty of real editors reject
`ACTION_SET_TEXT` while still honouring `ACTION_PASTE`, which is the path the system's own
long-press menu uses, so the server puts the text on the clipboard and pastes rather than
giving up. `data.method` says which path succeeded, and the user's prior clipboard contents
must be restored afterwards. Only when both are refused is this an `internal` error.

An action the platform **refuses** is a failure, not a success. Android returns a boolean
from `dispatchGesture`, `performAction`, and `performGlobalAction`; when it is false, nothing
touched the screen, and the server MUST answer `{"ok": false, "error": {"code": "internal", ...}}`
rather than `{"dispatched": true}`. Reporting a refused tap as dispatched is worse than an
error, because the client then reasons about a screen that never changed.

## Operations

| op | args | data (on ok) | notes |
|---|---|---|---|
| `ping` | (none) | `{ "version": "1", "service_connected": bool }` | health probe; never fails except `internal` |
| `screenshot` | `{ "max_width"?: int=540 }` | `{ "png_base64": string, "width": int, "height": int }` | AccessibilityService.takeScreenshot; downscale so width≤max_width; PNG. Enforce cap below. |
| `read` | `{ "max_depth"?: int=15 }` | `{ "foreground_package": string, "system_dialog": bool, "dialog"?: {"kind": string, "buttons": [string]}, "nodes": [Node] }` | UI-tree read |
| `foreground` | (none) | `{ "package": string, "activity"?: string }` | current foreground app |
| `tap` | `{ "x": int, "y": int }` OR `{ "text": string }` | `{ "dispatched": true }` | coordinate tap via dispatchGesture, or tap-by-visible-text (walk to clickable ancestor) |
| `swipe` | `{ "x1": int, "y1": int, "x2": int, "y2": int, "duration_ms"?: int=300 }` | `{ "dispatched": true }` | dispatchGesture stroke |
| `scroll` | `{ "direction": "forward"\|"backward"\|"up"\|"down", "x"?: int, "y"?: int }` | `{ "dispatched": true }` | prefer ACTION_SCROLL_* on nearest scrollable, else gesture |
| `text` | `{ "text": string }` | `{ "set": true, "method": "set_text"\|"paste" }` | ACTION_SET_TEXT on the input-focused node; `no_focus` if none. On refusal, falls back to clipboard + ACTION_PASTE and reports which path worked; `internal` only if both fail |
| `key` | `{ "key": "back"\|"home"\|"recents"\|"enter" }` | `{ "dispatched": true }` | global actions / key events |
| `launch` | `{ "package": string, "activity"?: string }` | `{ "launched": true }` | launch intent for package |
| `device` | `{ "what": "sensors"\|"location"\|"telephony" }` | the requested reading | read-only platform facts; answered by the socket server directly, since none of them need the accessibility service |
| `dialog` | `{ "button": string }` | `{ "handled": true }` | click a system-dialog button; synonym-expand ("allow" → Allow/ALLOW/"While using the app"/"Only this time") |

### Node (from `read`)

```json
{
  "text"?: string,
  "desc"?: string,          // contentDescription
  "class"?: string,         // e.g. android.widget.Button
  "resource_id"?: string,
  "clickable": bool,
  "editable": bool,
  "bounds": { "l": int, "t": int, "r": int, "b": int },
  "center": { "x": int, "y": int }   // precomputed tap point
}
```

Emit a node only if it has non-empty text OR desc OR is clickable OR is editable
(mirrors CellClaw's traverseNode filter). Depth-cap at `max_depth`.

## Device facts

`device` is the one op that does not touch the screen. It reports platform state the companion app
can read through ordinary Android APIs, and is answered by the socket server itself rather than
brokered to the accessibility service. Routing it there would add a Binder round trip and make
it fail whenever the service is switched off.

A permission the user has not granted comes back as `{"error": "<PERMISSION> not granted"}` inside
an **ok** response: the call succeeded and the answer is "you may not have this". Clients must
surface that as a failure rather than as data, or an ungranted location reads as a location fix.

## Blank frames (screenshot)

The server MUST refuse to return a frame with nothing in it. Slow-rendering apps are routinely
captured mid-draw, and a blank image is the worst possible output: the client cannot tell
"nothing rendered" from "nothing is there", so a vision model will fill the gap with plausible
fiction rather than retry. When the capture is overwhelmingly one flat colour (the reference
server samples on a grid and uses a 92% threshold, chosen because the status and nav bars always
render), answer `screenshot_failed` with a message saying the frame was blank, so the client
captures again.

## Foreground assertion (client-side)

The screen is shared mutable state owned by the user and the system, not by the agent: a
notification, a launcher gesture, or the agent's own stray tap can change the foreground app
between two calls. Clients should therefore state which app they believe they are driving and
verify it with `foreground` before acting or capturing, rather than assuming continuity. The Rust
tools expose this as an `expect_package` argument and refuse the call on a mismatch; no new op is
needed, since `foreground` already provides the fact.

## Size caps (screenshot)

- Downscale to `max_width` (default 540, matching CellClaw) preserving aspect ratio.
- The base64 payload MUST be ≤ 2 MiB (`MAX_BASE64_BYTES`, mirrors
  `zeroclaw-tools/src/screenshot.rs`). If still larger after downscale, reduce width
  further until it fits; if it cannot, return `screenshot_failed`.

## Client-side behavior (Rust): normative for `android_screenshot`

On `screenshot` success, the Rust tool writes the decoded PNG bytes to a temp file
(under the OS temp dir) and appends **`[IMAGE:<absolute-path>]`** on its own line to the
ToolResult output text, so the existing multimodal pipeline
(`zeroclaw-providers/src/multimodal.rs`) routes it to the vision model. Follow the
`crates/zeroclaw-tools/src/image_info.rs` marker pattern exactly (absolute path, on its
own line). Do NOT emit a bare `data:` URI (that is the existing ScreenshotTool bug).

## Overlay coordination (server)

If the bridge draws any overlay/bubble, hide it ~150 ms before `screenshot` and before a
coordinate `tap`, then restore; otherwise it appears in captures and intercepts taps
(CellClaw's OverlayVisibilityController pattern). If there is no overlay, this is a no-op.

## Timeouts

- Server should answer within 10 s; the a11y broker uses a 10 s `withTimeout` and returns
  `timeout` on expiry (CellClaw's default). The Rust client uses a 15 s read timeout on
  the socket and surfaces a graceful tool error (never panics) if the bridge is
  unreachable or slow.

## Versioning

`ping.version` is `"1"`. Additive fields are allowed without a version bump; removing or
renaming a field or op is a breaking change and requires bumping this doc + both sides.

## Conformance

The Rust client and the reference Kotlin server were checked against this document
op-by-op: all twelve operations, the `key` set (`back`, `home`, `recents`, `enter`), every
error code in the closed set, and the 10 s server / 15 s client timeout pair. The full loop
(launch, tree read, scroll, tap, screenshot into a vision model) has been exercised on a
physical unrooted device with nothing tethered.
