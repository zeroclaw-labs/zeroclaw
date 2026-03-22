---
name: browser
description: Headless browser automation using agent-browser CLI
metadata: {"zeroclaw":{"emoji":"🌐","requires":{"bins":["agent-browser"]}}}
---

# Browser Skill

Control a headless browser for web automation, scraping, and testing.

## Prerequisites

- `agent-browser` CLI installed globally (`npm install -g agent-browser`)
- Chrome downloaded (`agent-browser install`)

## Installation

```bash
# Install agent-browser CLI
npm install -g agent-browser

# Download Chrome for Testing
agent-browser install --with-deps  # Linux
agent-browser install              # macOS/Windows
```

## Usage

### Navigate and snapshot
```bash
agent-browser open https://example.com
agent-browser snapshot -i
```

### Interact with elements
```bash
agent-browser click @e1           # Click by ref
agent-browser fill @e2 "text"     # Fill input
agent-browser press Enter         # Press key
```

### Extract data
```bash
agent-browser get text @e1        # Get text content
agent-browser get url             # Get current URL
agent-browser screenshot page.png # Take screenshot
```

### Session management
```bash
agent-browser close               # Close browser
```

## Common Workflows

### Login flow
```bash
agent-browser open https://site.com/login
agent-browser snapshot -i
agent-browser fill @email "user@example.com"
agent-browser fill @password "secretpass"
agent-browser click @submit
agent-browser wait --text "Welcome"
```

### Scrape page content
```bash
agent-browser open https://news.ycombinator.com
agent-browser snapshot -i
agent-browser get text @e1
```

### Take screenshots
```bash
agent-browser open https://google.com
agent-browser screenshot --full page.png
```

## Options

- `--json` - JSON output for parsing
- `--headed` - Show browser window (for debugging)
- `--session-name <name>` - Persist session cookies
- `--profile <path>` - Use persistent browser profile

## Configuration

Enable browser in your ZeroClaw config:

```toml
[browser]
enabled = true
allowed_domains = ["*"]
backend = "agent_browser"
native_headless = true
```

Add `agent-browser` to allowed commands:

```toml
allowed_commands = [
    "agent-browser",
    # ... other commands
]
```

Add to auto-approve for seamless operation:

```toml
auto_approve = [
    "browser",
    # ... other tools
]
```

## Full Command Reference

Run `agent-browser --help` for all available commands.

## Related

- [agent-browser GitHub](https://github.com/vercel-labs/agent-browser)
- [VNC Setup Guide](../docs/browser-vnc-setup.md)
