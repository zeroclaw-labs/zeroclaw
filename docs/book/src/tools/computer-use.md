# Computer Use

Computer use lets an agent inspect and operate applications in the logged-in
desktop session. It is separate from [browser automation](./browser.md): browser
tools use DOM and accessibility semantics inside web pages, while computer use
targets arbitrary local applications.

The first driver is for macOS. Linux follows with separate X11 and Wayland
adapters; Windows follows with UI Automation and native input support.

| Platform | Status | Primary semantic interface | Coordinate fallback |
|---|---|---|---|
| macOS | Experimental | Accessibility (AX) | CoreGraphics events |
| Linux | Planned next | AT-SPI | XTest for X11; portal/libei for Wayland |
| Windows | Planned | UI Automation | `SendInput` |

## Safety model

The capability is off at two independent boundaries:

- The main binary must be built with the `computer-use` Cargo feature.
- `[computer_use].enabled` must be set to `true` in config.

Desktop access has a stronger rule than ordinary tool approval. Whole-screen
capture and every action that focuses an application, presses an accessibility
element, moves or clicks the pointer, scrolls, or sends keyboard input use the
configured confirmation mode:

- `fresh` is the default. Every input action requires a new operator decision;
  full autonomy, `auto_approve`, and a prior **Always** response cannot bypass
  it. Each call must be issued alone so approval immediately precedes execution.
- `session` follows the ordinary risk profile. In supervised mode, choosing
  **Always** admits subsequent `computer_use` calls for that runtime session;
  `auto_approve` and full autonomy can also admit them. Calls remain ordered
  and execute one at a time.

Application access is a separate decision. `allowlist` is the fail-closed
default and admits only exact selectors in `allowed_applications`. `desktop`
explicitly admits any valid application identity resolved by the operating
system. The literal `*` is rejected; desktop-wide control must be named
directly so it cannot be mistaken for an application selector.

The existing standalone `screenshot` tool uses the same fresh-confirmation
rule for whole-screen capture, so it cannot be used as an unapproved fallback.

`list_apps` returns a bounded inventory of running GUI application identities.
AX inspection results can contain document text, messages, filenames, and
other content exposed by an admitted application. The live result is sent to
the configured model provider and becomes part of the conversation/tool-result
data path. Password/secure-field values are suppressed, but ordinary AX text
is not. Whole-screen screenshots are likewise sent to a vision-capable model
when the returned image marker is consumed. Choose the application allowlist
and model provider with that disclosure in mind.

Treat all inspected UI text and screenshot pixels as untrusted data, never as
agent instructions. An application or web page can deliberately render prompt
injection text. Do not follow UI-authored requests to reveal secrets, change
policy, bypass confirmation, or operate another target; only the operator's
request and the configured policy authorize an action.

Inspection node titles, values, and descriptions are excluded from ZeroClaw
logs, observers, receipts, hooks, and SOP capture. Those surfaces receive only
a bounded audit projection (application identity, node count, request limits,
and truncation state); the model-facing result is not duplicated into them.

The resolved application policy is checked again by the driver immediately
before each input event. In allowlist mode, an empty `allowed_applications`
list denies application discovery, inspection, and all input. Desktop mode
still requires an exact target name or bundle identifier on every action and
revalidates the operating-system identity before input. Prefer `press_element`
with an application, role, and title; raw coordinates are less auditable and
should be a fallback.

The native driver is private to the feature-gated tool and is not exposed as a
standalone executable. This keeps caller-resolved policy and the trusted
approval decision inside one process instead of accepting forgeable policy over
an external command boundary. Screenshot bytes are written to a uniquely named
PNG under the active agent workspace. The tool returns an explicit
`[IMAGE:<path>]` marker so a vision-capable model can inspect it. These files
are workspace artifacts and remain until the operator removes them; they are
not long-term memory entries.

This confirmation boundary is not an operating-system sandbox. If the same
agent is separately allowed to execute arbitrary native code through `shell`,
cron shell jobs, or a code-runner tool, that code may call desktop APIs under
the agent's OS identity without going through `computer_use`. For a hard
desktop-control boundary, deny those arbitrary-code tools in the agent's tool
policy and run ZeroClaw under a dedicated, sandboxed OS account. Do not treat
this experimental preview as a multi-tenant security boundary.

Do not use `type_text` for passwords, API keys, recovery codes, or other
secrets. Intended text is deliberately shown in the approval prompt and may be
present in approval/observability records; enter credentials manually through
the target application's secure input surface.

## Build the macOS preview

The bundled macOS desktop installer includes the native computer-use feature.
For a source build, enable it explicitly:

```sh
cargo build --release --features computer-use
```

## Configure desktop-wide control

For Hermes-like target discovery and one approval that can last for the
runtime session:

```toml
[computer_use]
enabled = true
application_access = "desktop"
confirmation_mode = "session"
timeout_ms = 15000
max_text_chars = 80
```

On the first policy-gated action, choose **Always** to approve `computer_use`
for the session. This grants every action against every exact application
target for that session, not only the target displayed in the first prompt.
In supervised mode, use `always_ask = ["computer_use"]` in the active risk
profile when every call should continue to prompt. Full autonomy takes
precedence over `always_ask` and does not prompt.

Desktop-wide control is not background automation in the current macOS
driver. Focusing and input actions activate the target application and use the
real system pointer/keyboard event stream. No-focus background routing is a
separate driver capability planned after the initial platform rollout.

## Configure an application allowlist

For the restrictive mode, keep the defaults and name exact applications:

```toml
[computer_use]
enabled = true
application_access = "allowlist"
confirmation_mode = "fresh"
allowed_applications = ["com.apple.Safari", "com.apple.Preview"]
```

Optional `min_coordinate_x`, `min_coordinate_y`, `max_coordinate_x`, and
`max_coordinate_y` values constrain the global logical coordinate space.
Negative minima support displays positioned to the left or above the main
display. `max_text_chars` may be 1 through 80 so the approval prompt always
shows the complete intended text; split longer content into separately approved
chunks.

Prefer stable bundle identifiers such as `com.apple.Safari` over display names
in `allowed_applications`. Reverse-DNS-shaped selectors match only the bundle
identifier namespace; other selectors match only the display-name namespace,
so one cannot impersonate the other. Display-name matching is still a
convenience, not a signed application-identity guarantee; hardened deployments
must also rely on OS account isolation and code-signing policy.

After changing config, reload the daemon so its tool registry is rebuilt.

## Grant macOS permissions

ZeroClaw runs in the current user's graphical session and needs two macOS
privacy grants (plus Automation consent when macOS prompts for System Events):

1. Open **System Settings → Privacy & Security → Accessibility** and allow the
   terminal or packaged application that launches `zeroclaw`.
2. Open **System Settings → Privacy & Security → Screen & System Audio
   Recording** and grant the same launcher.
3. The first AX inspection or semantic press may ask whether the launcher may
   control **System Events**. Approve it under **Privacy & Security →
   Automation**.
4. Quit and restart ZeroClaw after changing a grant.

Permission is attached to the launching executable or application. A binary
started by a terminal and the same binary started by a LaunchAgent may not have
the same effective grant. Computer use also requires a logged-in WindowServer
session; it cannot operate a machine sitting at the pre-login screen or a
headless system service.

Use the `capabilities` action first when troubleshooting. It reports the
compiled platform and current permission availability without sending input.

## macOS actions

- `capabilities`: report driver capabilities and permission readiness.
- `list_apps`: return a bounded, policy-filtered inventory of running GUI
  applications without exposing their names to audit or observability logs.
- `inspect`: return a bounded AX tree only when the exact requested admitted
  application is frontmost.
- `screenshot`: after the configured approval, activate an exact admitted
  application, capture the main display, and return frame metadata plus an
  image marker.
- `press_element`: find one AX element by application, title, and optional role,
  then perform its press action.
- `focus`, `mouse_move`, `click`, `scroll`, `type_text`, and
  `key_press`: controlled input fallbacks using the configured confirmation
  mode.

AX inspection is bounded by node count, depth, output size, and wall-clock
timeout. If an application exposes no usable accessibility data, take a
screenshot and use coordinates only after visually verifying the current
frame. Coordinate drag is intentionally not exposed in the first macOS slice:
a global drag path cannot yet prove that every affected surface belongs to the
approved application.

## Platform rollout

Linux is the next implementation target. X11 and Wayland are different trust
and input systems, so an unsupported compositor must return a clear error
instead of silently falling back. Windows follows after Linux; it will use UI
Automation first and respect process integrity/UIPI boundaries before falling
back to synthetic input.
