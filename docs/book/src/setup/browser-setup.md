# Browser Automation Setup Guide

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

```bash
# Install CLI
npm install -g agent-browser

# Download Chrome for Testing
agent-browser install --with-deps  # Linux (includes system deps)
agent-browser install              # macOS/Windows
```

### 2. Verify ZeroClaw Config

The browser tool is enabled by default. To verify or customize, edit
`~/.zeroclaw/config.toml`:

```toml
[browser]
enabled = true              # default: true
allowed_domains = ["*"]     # default: ["*"] (all public hosts)
backend = "agent_browser"   # default: "agent_browser"
native_headless = true      # default: true
```

To restrict domains or disable the browser tool:

```toml
[browser]
enabled = false                              # disable entirely
# or restrict to specific domains:
allowed_domains = ["example.com", "docs.example.com"]
```

### 3. Test

```bash
echo "Open https://example.com and tell me what it says" | zeroclaw agent
```

## VNC Setup (GUI Access)

For debugging or when you need visual browser access:

### Install Dependencies

```bash
# Ubuntu/Debian
apt-get install -y xvfb x11vnc fluxbox novnc websockify

# Optional: Desktop environment for Chrome Remote Desktop
apt-get install -y xfce4 xfce4-goodies
```

### Start VNC Server

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

### VNC Access

- **VNC Client**: Connect to `localhost:5900`
- **Web Browser**: Open `http://localhost:6080/vnc.html`

### Start Browser on VNC Display

```bash
DISPLAY=:99 google-chrome --no-sandbox https://example.com &
```

## Chrome Remote Desktop

### Install

```bash
# Download and install
wget https://dl.google.com/linux/direct/chrome-remote-desktop_current_amd64.deb
apt-get install -y ./chrome-remote-desktop_current_amd64.deb

# Configure session
echo "xfce4-session" > ~/.chrome-remote-desktop-session
chmod +x ~/.chrome-remote-desktop-session
```

### Setup

1. Visit <https://remotedesktop.google.com/headless>
2. Copy the "Debian Linux" setup command
3. Run it on your server
4. Start the service: `systemctl --user start chrome-remote-desktop`

### Remote Access

Go to <https://remotedesktop.google.com/access> from any device.

## Testing

### CLI Tests

```bash
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

### ZeroClaw Integration Tests

```bash
# Content extraction
echo "Open https://example.com and summarize it" | zeroclaw agent

# Navigation
echo "Go to https://github.com/trending and list the top 3 repos" | zeroclaw agent

# Form interaction
echo "Go to Wikipedia, search for 'Rust programming language', and summarize" | zeroclaw agent
```

## Troubleshooting

### "Element not found"

The page may not be fully loaded. Add a wait:

```bash
agent-browser open https://slow-site.com
agent-browser wait --load networkidle
agent-browser snapshot -i
```

### Cookie dialogs blocking access

Handle cookie consent first:

```bash
agent-browser open https://site-with-cookies.com
agent-browser snapshot -i
agent-browser click @accept_cookies  # Click the accept button
agent-browser snapshot -i  # Now get the actual content
```

### Docker sandbox network restrictions

If `web_fetch` fails inside Docker sandbox, use agent-browser instead:

```bash
# Instead of web_fetch, use:
agent-browser open https://example.com
agent-browser get text body
```

## Security Notes

- `agent-browser` runs Chrome in headless mode with sandboxing
- For sensitive sites, use `--session-name` to persist auth state
- The `--allowed-domains` config restricts navigation to specific domains
- VNC ports (5900, 6080) should be behind a firewall or Tailscale

## Related

- [agent-browser Documentation](https://github.com/vercel-labs/agent-browser)
- [ZeroClaw Configuration Reference](./config-reference.md)
- [Skills Documentation](../skills/)
