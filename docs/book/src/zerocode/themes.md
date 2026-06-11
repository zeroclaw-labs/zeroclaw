# Themes & terminal colors

zerocode ships a set of named color themes, lets each agent carry its own theme
in the Code and Chat panes, and adapts its color output to what the terminal
can render.

All theme settings live in zerocode's own config, independent of
which daemon zerocode connects to.

## Choosing a theme

Open the **Config** pane, switch to the **zerocode** section, and select the
**Theme** tab. Navigate with `↑`/`k` and `↓`/`j`; press `Enter` to apply. The
choice takes effect immediately and is persisted to zerocode's config. The
highlighted row previews the theme's palette inline, a strip of color
blocks for its canvas, title, heading, body, warn, and tool roles, so you can
see the colors before applying.

### Available themes

Two themes are authored directly in zerocode because they have no registry
entry:

- **`terminal`**: inherits your terminal's own colors. Every role is left to
  the terminal default and the app skips painting a background, so a tuned shell
  palette shows through untouched.
- **`icy_blue`**: the default on non-macOS platforms.

The remaining themes are generated at build time from the dashboard theme
registry (`web/src/contexts/themes.json`), the single source of truth zerocode,
the React dashboard, and these docs all read. The list below is produced by
`cargo xtask mdbook themes` and included at build time. It is never committed:

{{#include zerocode-theme-list.md}}

The default theme is `terminal` on macOS and `icy_blue` on every other
platform.

> If `[theme].name` (or a per-agent override below) names a theme this build
> does not have, a typo, or a config written by a newer build, zerocode falls
> back to the `terminal` theme rather than refusing to start.

## Per-agent themes (Code & Chat panes)

A theme can follow the agent. When the **Code** or **Chat** pane is focused on
an agent that has an override, that agent's theme replaces the base theme while
the pane is shown; other panes keep the base theme. This makes it easy to tell
at a glance which agent you are working with.

Configure overrides under `[theme.agent_override.<alias>]`:

### Setting overrides from the UI

In the Config pane's zerocode section, open the **Agent Themes** tab. It lists
the daemon's enabled agents with each agent's current override (or `—` for
none). The footer shows the keys.

- `Enter` on an agent opens the **Theme** list in assign mode (titled
  `Theme → <agent>`); pick a theme and it is assigned to that agent.
- `d` clears the highlighted agent's override.

Assignments and clears apply **live**: the Code/Chat pane re-themes on the
next frame without restarting zerocode, and are persisted to zerocode's
config. An override naming an unknown theme falls back to the
`terminal` theme, same as the global setting.

## Terminal color depth

zerocode detects how much color your terminal can render and adapts its output
once, on first paint:

- **Truecolor (24-bit)** is the default for virtually every terminal. zerocode
  emits 24-bit color, which modern terminals, and terminal multiplexers
  configured for it, render directly.
- **xterm-256** is used for macOS Terminal.app, which lacks 24-bit color
  (detected via `TERM_PROGRAM=Apple_Terminal`). iTerm2, kitty, WezTerm, Ghostty,
  and other truecolor terminals are unaffected.
- **ANSI-16** is used only for a genuinely low-color terminal: `TERM` unset, or
  `dumb`, `ansi`, or any `*-16color` value. Themed colors are down-converted to
  the nearest of the 16 ANSI colors.

When a depth below truecolor is in effect, every themed color (including the
preview swatches) is snapped to the nearest renderable value.

### Forcing a depth

Set `ZEROCODE_COLOR` to override detection:

| Value | Depth |
|---|---|
| `truecolor`, `24bit`, `24` | 24-bit truecolor |
| `256` | xterm-256 |
| `16`, `ansi` | 16 ANSI colors |

<div class="os-tabs-src">

#### sh

```sh
ZEROCODE_COLOR=256 zerocode
```

</div>

Any unrecognised value is ignored and normal detection runs.

### tmux: colors look wrong or washed out

If you run zerocode inside tmux, commonly over SSH to a Raspberry Pi or other
remote host, and the palette renders flat or near-monochrome, the cause is
almost always tmux not advertising truecolor (RGB) support for the outer
terminal. zerocode emits 24-bit color, but tmux only forwards it to your
terminal when it knows the terminal supports RGB.

Tell tmux the client supports RGB by adding this to the **remote** machine's
`~/.tmux.conf`:

```tmux
set -as terminal-features ",*:RGB"
```

Then restart the tmux server so the change takes effect:

<div class="os-tabs-src">

#### sh

```sh
tmux kill-server
```

</div>

Reattach, and zerocode's truecolor output will render correctly.

As an alternative that needs no tmux change, force a lower depth that does not
rely on truecolor passthrough:

<div class="os-tabs-src">

#### sh

```sh
ZEROCODE_COLOR=256 zerocode
```

</div>
