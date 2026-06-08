# Quickstart

Quickstart is the guided setup that takes you from a fresh install to a working
agent in one pass. It runs on three surfaces: the **CLI**, the **zerocode**
terminal interface, and the **web gateway**. All three drive the same
underlying flow, so the config they produce is identical. Use whichever fits
where you are.

## Install

Pick one:

**Linux / macOS (one-liner):**

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

**Homebrew (macOS, Linux):**

```bash
brew install zeroclaw
```

**Windows:** run `setup.bat` from the latest release, or see
[Setup → Windows](../setup/windows.md).

**From source** (inside a clone):

```bash
./install.sh
```

This builds and installs both `zeroclaw` and the `zerocode` terminal interface.
Run it with no flags for an interactive picker that lets you choose the build
type, which apps to install, and which optional features to compile in.

## The steps

> **Important:** if any of these terms are unfamiliar, read
> [Getting Started → Concepts](./index.md#concepts) first. It defines model
> provider, risk profile, alias, and the rest in one place.

{{#include ../_snippets/quickstart-steps.md}}

## CLI

The fastest path on a headless box or over SSH:

```bash
zeroclaw quickstart
```

You answer one prompt per step in the terminal. The built-in `cli` channel
works immediately, so Channels and Peer groups can be skipped. For an
all-defaults, no-approvals config, see [YOLO mode](./yolo.md).

## zerocode

In the [zerocode](./tui.md) terminal interface, the Quickstart pane is one of
the tabs. Drive it with the keyboard:

Switch to the **Quickstart** pane:

{{#include ../_snippets/zerocode-pane-nav-keys.md}}

Inside the pane:

{{#include ../_snippets/zerocode-quickstart-pane-keys.md}}

Mouse works too: click a tab in the mode bar to switch panes, click a step to
select and open it, and scroll to move through the list.

Each step opens a modal that mirrors the checklist above, with a "Use existing"
option that lists the matching aliases already in your config.

## Web gateway

With the daemon running, open the dashboard in a browser:

```bash
zeroclaw daemon
```

`zeroclaw daemon` runs the full runtime: the gateway, your configured channels,
the scheduler, and the heartbeat monitor. (`zeroclaw gateway` starts only the
HTTP gateway if that is all you need.)

Then visit `http://127.0.0.1:42617/quickstart`. A fresh install with no agents
configured redirects there automatically; afterward you can always reach it
from the dashboard navigation.

The web form presents the same steps as cards. On submit it applies your
submission through the daemon (`POST /api/quickstart/apply`), which returns a
structured error list if anything is invalid, then reloads the daemon in place
so the new agent is live without a restart. A separate
`POST /api/quickstart/validate` endpoint runs the same checks without applying,
for clients that want to validate first.

## After Quickstart

- **Drive it from [zerocode](./tui.md):** the terminal interface is the best
  way to chat, watch live logs, manage config, and monitor the daemon, all in
  one place. Just run `zerocode`.
- **Quick one-off from the shell:** `zeroclaw agent -a <alias> -m "your message"`.
- **Run always-on:** `zeroclaw service install && zeroclaw service start`.
- **Add channels later:** [Channels → Overview](../channels/overview.md).
- **Tune autonomy and budgets:** [Reference → Config](../reference/config.md).
