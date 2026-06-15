# Browser Automation

This guide covers setting up browser automation capabilities in ZeroClaw, including both headless automation and GUI access via VNC.

## Overview

ZeroClaw supports multiple browser access methods:

| Method | Use Case | Requirements |
|--------|----------|--------------|
| **agent-browser CLI** | Headless automation, AI agents | npm, Chrome |
| **VNC + noVNC** | GUI access, debugging | Xvfb, x11vnc, noVNC |
| **Chrome Remote Desktop** | Remote GUI via Google | XFCE, Google account |

## Quick Start: Headless Automation

### 1. Install agent-browser

<div class="os-tabs-src">

#### Linux

```sh
# Install CLI
npm install -g agent-browser

# Download Chrome for Testing (includes system deps)
agent-browser install --with-deps
```

#### macOS / Windows

```sh
# Install CLI
npm install -g agent-browser

# Download Chrome for Testing
agent-browser install
```

</div>

### 2. Verify ZeroClaw Config

The browser tool is enabled by default with `allowed_domains = ["*"]`. Restrict domains or disable it via `zeroclaw config set`:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw config set browser.allowed_domains '["example.com", "docs.example.com"]'
zeroclaw config set browser.enabled false
```

</div>

{{#config-where browser}}

For the `agent_browser` backend, set `browser.headed = true` to launch the browser in headed mode for debugging or first-time login setup, or `browser.headed = false` to force headless mode. When `browser.headed` is unset, Zeroclaw preserves the inherited `AGENT_BROWSER_HEADED` environment behavior. The rust-native backend continues to use `browser.native_headless`.

See the [Config reference](../reference/config.md) for all browser fields and defaults.

### 3. Test

<div class="os-tabs-src">

#### sh

```sh
echo "Open https://example.com and tell me what it says" | zeroclaw agent -a assistant
```

</div>

## VNC Setup (GUI Access)

For debugging or when you need visual browser access:

### Install Dependencies

<div class="os-tabs-src">

#### Debian/Ubuntu

```sh
apt-get install -y xvfb x11vnc fluxbox novnc websockify

# Optional: Desktop environment for Chrome Remote Desktop
apt-get install -y xfce4 xfce4-goodies
```

</div>

### Start VNC Server

<div class="os-tabs-src">

#### bash

```bash
#!/bin/bash
# Start virtual display with VNC access

DISPLAY_NUM=99
VNC_PORT=5900
NOVNC_PORT=6080
RESOLUTION=1920x1080x24

# Start Xvfb
Xvfb :$DISPLAY_NUM -screen 0 $RESOLUTION -ac &
sleep 1

# Start window manager
fluxbox -display :$DISPLAY_NUM &
sleep 1

# Start x11vnc
x11vnc -display :$DISPLAY_NUM -rfbport $VNC_PORT -forever -shared -nopw -bg
sleep 1

# Start noVNC (web-based VNC)
websockify --web=/usr/share/novnc $NOVNC_PORT localhost:$VNC_PORT &

echo "VNC available at:"
echo "  VNC Client: localhost:$VNC_PORT"
echo "  Web Browser: http://localhost:$NOVNC_PORT/vnc.html"
```

</div>

### VNC Access

- **VNC Client**: Connect to `localhost:5900`
- **Web Browser**: Open `http://localhost:6080/vnc.html`

### Start Browser on VNC Display

<div class="os-tabs-src">

#### sh

```sh
DISPLAY=:99 google-chrome --no-sandbox https://example.com &
```

</div>

## Chrome Remote Desktop

### Install

<div class="os-tabs-src">

#### sh

```sh
# Download and install
wget https://dl.google.com/linux/direct/chrome-remote-desktop_current_amd64.deb
apt-get install -y ./chrome-remote-desktop_current_amd64.deb

# Configure session
echo "xfce4-session" > ~/.chrome-remote-desktop-session
chmod +x ~/.chrome-remote-desktop-session
```

</div>

### Setup

1. Visit <https://remotedesktop.google.com/headless>
2. Copy the "Debian Linux" setup command
3. Run it on your server
4. Start the service: `systemctl --user start chrome-remote-desktop`

### Remote Access

Go to <https://remotedesktop.google.com/access> from any device.

## Testing

### CLI Tests

<div class="os-tabs-src">

#### sh

```sh
# Basic open and close
agent-browser open https://example.com
agent-browser get title
agent-browser close

# Snapshot with refs
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser close

# Screenshot
agent-browser open https://example.com
agent-browser screenshot /tmp/test.png
agent-browser close
```

</div>

### ZeroClaw Integration Tests

<div class="os-tabs-src">

#### sh

```sh
# Content extraction
echo "Open https://example.com and summarize it" | zeroclaw agent -a assistant

# Navigation
echo "Go to https://github.com/trending and list the top 3 repos" | zeroclaw agent -a assistant

# Form interaction
echo "Go to Wikipedia, search for 'Rust programming language', and summarize" | zeroclaw agent -a assistant
```

</div>

## Troubleshooting

### "Element not found"

The page may not be fully loaded. Add a wait:

<div class="os-tabs-src">

#### sh

```sh
agent-browser open https://slow-site.com
agent-browser wait --load networkidle
agent-browser snapshot -i
```

</div>

### Cookie dialogs blocking access

Handle cookie consent first:

<div class="os-tabs-src">

#### sh

```sh
agent-browser open https://site-with-cookies.com
agent-browser snapshot -i
agent-browser click @accept_cookies  # Click the accept button
agent-browser snapshot -i  # Now get the actual content
```

</div>

### Docker sandbox network restrictions

If `web_fetch` fails inside Docker sandbox, use agent-browser instead:

<div class="os-tabs-src">

#### sh

```sh
# Instead of web_fetch, use:
agent-browser open https://example.com
agent-browser get text body
```

</div>

## Security Notes

- `agent-browser` runs Chrome in headless mode with sandboxing
- For sensitive sites, use `--session-name` to persist auth state
- The `--allowed-domains` config restricts navigation to specific domains
- VNC ports (5900, 6080) should be behind a firewall or Tailscale

## Related

- [agent-browser Documentation](https://github.com/vercel-labs/agent-browser)
- [Config reference](../reference/config.md)
