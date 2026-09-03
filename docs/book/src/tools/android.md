# Android-native tools

ZeroClaw can run as an ordinary app on an Android phone, but an ordinary app
UID is denied `screencap` and input injection. It therefore cannot see or touch
the screen on its own.

The `android_*` tool family closes that gap. The ZeroClaw Android APK holds an
`AccessibilityService` and exposes UI control plus read-only device facts over
an app-private Unix-domain socket to its bundled ZeroClaw process. The tools let
the agent look at the screen, read what is on it, act on it, and inspect selected
Android APIs.

The family is **off by default** and only registers when ZeroClaw is genuinely
running on Android. See [Bridge protocol](./android-bridge-protocol.md) for the
wire contract between the two halves.

## Tools

| Tool | What it does |
|---|---|
| `android_screenshot` | Capture the screen and attach it for visual inspection. The image reaches the vision model through an `[IMAGE:<path>]` marker, the same path used by `image_info`. |
| `android_ui_read` | Read the accessibility tree: visible text, content descriptions, resource IDs, and a precomputed tap point for every interactive node. |
| `android_action` | Tap, swipe, scroll, type text, or press a navigation key in an explicitly named foreground app. |
| `android_dialog` | Allow or deny a recognized system permission/install dialog through a separately approval-gated path. |
| `android_launch` | Launch an app by package name, optionally targeting a specific activity. |
| `android_device` | Read a device fact: attached sensors, last known location, or telephony/carrier state. Reads only, and needs no accessibility service. |

The usual loop is `android_screenshot` or `android_ui_read` to find a target,
then `android_action` to act on it. `android_ui_read` is normally the cheaper
of the two: it returns tap coordinates directly, without spending image tokens.

How much the tree is worth varies sharply by app. Apps built from standard
widgets expose useful labels; image-heavy and custom-drawn apps often expose
almost nothing, and there the screenshot is the only way to find a control. When
the tree comes back thin, capture the screen rather than guessing coordinates.

## Configuration

```toml
[android]
# Enable the android_* tools. Also requires the process to be running on
# Android; on any other platform nothing is registered.
enabled = true

# The Android app's bridge socket, inside its private files directory.
socket_path = "/data/data/org.zerodroid.bridge/files/ui.sock"

# Refuse to register android_action when the active risk profile would
# auto-approve it. See "Approval" below.
require_approval_for_actions = true

# Screenshots are downscaled to this width before encoding.
screenshot_max_width = 540
```

Every field has a safe default; `enabled = true` is the only line strictly
required in operator-authored config for the primary Android user. App-generated
config always writes the actual `filesDir` path, including the correct
`/data/user/<id>/` prefix for secondary users and work profiles.

## Security model

Two independent conditions must both hold before any tool is registered:

1. `[android] enabled = true`, and
2. `zeroclaw_api::platform::is_android()` returns true.

If the config opts in but the process is not on Android, registration is
skipped with a warning rather than silently registering tools that could never
succeed.

The transport has **no token and no auth field**, by design. The socket lives
inside the bridge APK's private files directory, so kernel UID isolation is the
trust boundary: no other app UID can open it. Do not relocate the socket
somewhere world-readable.

### Approval

`android_action` mutates device state, so it is deliberately **absent from the
default `auto_approve` list**. Under supervised autonomy an unlisted tool falls
through to a prompt, which is what puts a human in front of every tap.

`require_approval_for_actions = true` (the default) additionally fails closed:
if the active risk profile would auto-approve `android_action`, explicitly or
through a `"*"` wildcard, and `always_ask` does not pull it back, the tool is
not registered at all, and a warning explains why. Set it to `false` only if
you genuinely intend unattended control of the device.

`android_dialog` is never part of ordinary autonomous control. If its effective
profile would auto-approve it (including full autonomy or `"*"`), it is omitted
from the registry. This keeps permission and package-install confirmation
available to an approval-capable session without letting a broad UI-control
grant inherit that privilege.

`android_screenshot` and `android_ui_read` only read. `android_launch` and
`android_action` both pass the autonomy and rate-limit gate, so read-only
autonomy blocks them.

### The generic `screenshot` tool on Android

`screenshot` captures by shelling out to `screencapture` (macOS) or `gnome-screenshot` / `scrot` /
`import` (Linux). None of those exist on Android, and an app UID may not run them, so on that
platform the tool would answer "Screenshot not supported" to every call. Where the bridge is
available it is wired through to the same capture path as `android_screenshot`, including the
`expect_package` guard, so the familiar name works rather than failing. Offering a second name for
one job must not offer a second, unguarded way to do it.

### Acting in the wrong app

`android_action` requires `expect_package`; `android_dialog` requires the expected
system package. The Rust client checks first for useful feedback, then the
AccessibilityService revalidates at the final mutation boundary. If the screen
changes between those checks, the action fails instead of landing in the new
foreground app. Ordinary actions are refused while a system dialog is showing.
`android_screenshot` accepts the same field as an optional observation guard.

`android_screenshot` also fails rather than returning a blank capture. Slow apps are often caught
mid-render, and an empty image is worse than an error: there is no way to distinguish "nothing
rendered" from "nothing is there", so it invites a confident description of a screen that was
never read. Treat `blank frame` as "capture again", not as "the screen is empty".

### Untrusted screen content

Whatever is on the screen belongs to somebody else. `android_ui_read` and
`android_screenshot` feed arbitrary third-party app content into the model's
context, so any text a page, message, or advertisement puts on screen is
untrusted input that may try to steer the agent. The exposure class is the same
as the browser tools: read output is capped and structured, and the one tool
that can act on a suggestion is the one behind an approval prompt. Keep it
there when the agent is looking at content the user did not write.

Password nodes never expose text or descriptions, and zerodroid's own
credential-bearing UI is excluded from accessibility reads and screenshots.
Captured PNGs live in a private 0700 cache with 0600 files and are bounded by
age, count, and bytes; stale captures are swept rather than retained forever.

## Failure modes

Bridge errors surface as ordinary tool errors, never panics. The most common
by far is the bridge not being reachable, which usually means the Android app
is not running or its accessibility service is switched off in system settings.

The client uses a 15 s read timeout against the server's 10 s budget, so a
server-side `timeout` is reported as such rather than being masked by the
client giving up first.

## Platform support

Production binaries compile the module only for Android. Unix-hosted unit tests
retain the protocol client for coverage; Linux, macOS, and Windows production
builds contain no Android tool implementation or registration path.
