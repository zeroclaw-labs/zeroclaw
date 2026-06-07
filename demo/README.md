# ZeroClaw ESP32 Smart Room Demo

Simulated ESP32 + ZeroClaw agent + browser visualization. Hardware-free.
The Docker path runs the simulator plus an in-container interactive agent. The
recommended Telegram path runs the simulator and channel agent directly on the
host to keep the setup light and explicit.

## Architecture

```
[Host browser] ──:8080──┐
                        │
   ┌────────────────────▼─────────────────────────┐
   │  Docker container "zeroclaw-demo"            │
   │  ┌───────────────┐    ┌────────────────────┐ │
   │  │ esp32_sim     │    │ zeroclaw (chat)    │ │
   │  │ • socat       │    │ • OpenRouter (your model) │ │
   │  │ • HTTP :8080  │    │ • smartroom tools  │ │
   │  │ • WS /ws      │    │   (set_device etc) │ │
   │  │ • pty master ─┼────┼─► pty slave        │ │
   │  └───────────────┘    └────────────────────┘ │
   │           shared /tmp + /dev/pts             │
   └──────────────────────────────────────────────┘
```

The simulator runs as the container's default process. The agent runs via
`docker compose exec` inside the same container so it can see the same `/tmp`
and `/dev/pts` namespace (necessary for pty handoff).

## Why a container

The agent's tool surface is heavily constrained (see `zeroclaw.toml.example`):
only the smartroom tools (`set_device` / `read_device`) plus raw `gpio_*` and
`hardware_capabilities` are available. All other surfaces (shell, browser,
web search, MCP, etc.) are disabled. For the Docker path, the container
provides defence in depth around the simulator and local interactive chat.

## Low-storage / MacBook Air development path (recommended for limited disk)

If you cannot allocate 60-80+ GB to Docker Desktop, test the vignette directly on your host instead. This is much lighter and lets you iterate quickly.

```bash
# 1. One-time setup
cp demo/.env.template demo/.env
nano demo/.env          # easiest on Mac. Use: code demo/.env  or  vim demo/.env
                        # At minimum, set:  OPENROUTER_API_KEY="your-real-key"
# For Telegram test comms: TELEGRAM_BOT_TOKEN="your-bot-token" (from @BotFather)

# 2. Install socat if missing (required by the simulator)
brew install socat

# 3. Ensure demo config exists
mkdir -p demo/data/config
cp -n demo/zeroclaw.toml.example demo/data/config/config.toml || true

# 4. Keep secrets in demo/.env. The host script injects them at runtime and does not persist them into config.toml.

# 5. Terminal 1 – start simulator + visualizer
./demo/run-sim-host.sh

# Wait for "frontend ready: http://127.0.0.1:8080", then open the URL. You should see the simulated room (lamps, fan, motion sensor etc.).

# 6. Terminal 2 – start the channel agent (it will poll Telegram if token configured)
./demo/run-agent-host.sh
```

The agent terminal prints a one-time Telegram bind code. From your Telegram chat with the bot, send `/bind <code>` first. After the bot confirms binding, paste the system primer from `demo/PROMPTS.md` and use natural language exactly as in the prompts (e.g. "It's getting dark and chilly. I'm settling in to read for an hour.")

The agent should use the smartroom tools, the commands go over the pty to the sim, and you see the visualizer update live.

This gives you the full functional vignette (smartroom tools → pty → simulator → live SVG) with far less disk pressure.

## Packaged demo (Docker) – requires decent disk

Use this path when you want the one-command simulator plus local interactive
agent experience in a container. It does not include the Telegram channel; use
the host path above for Telegram.

**Requirements**
- Docker Desktop with **at least 60-80 GB** allocated to the disk image (see Settings → Resources)
- An OpenRouter key for the LLM

**One-time setup**

```bash
cp demo/.env.template demo/.env
$EDITOR demo/.env   # set OPENROUTER_API_KEY
```

**Build (heavy first time)**

```bash
docker compose -f demo/docker-compose.yml build
```

See the "Run (two terminals)" section below.

## Run (two terminals) – Docker packaged path

**Terminal 1 — simulator + frontend:**
```bash
./demo/run-sim.sh
```
Wait for `frontend ready: http://127.0.0.1:8080` then open it.

**Terminal 2 — interactive chat:**
```bash
./demo/run-zeroclaw.sh
```

Paste the (updated) system primer from `demo/PROMPTS.md`, then use natural language.

The browser SVG updates live when the agent calls `set_device`.

> **Tip for low-storage machines:** Use the "Low-storage / MacBook Air development path" above instead of Docker. It runs the same vignette with `cargo run` directly on your host and uses far less disk.

## Public URL via ngrok (for the hackathon demo)

```bash
brew install ngrok
ngrok config add-authtoken <TOKEN>   # from ngrok dashboard
ngrok http 8080
```

Share the `https://xxxx.ngrok-free.app` URL — the audience can pull it up on
their phones.

## Stop / clean up

```bash
cd demo && docker compose down
```

To wipe the cargo build cache (forces a fresh build):
```bash
docker builder prune
```

## Files

```
demo/
├── README.md            ← this file
├── Dockerfile           ← multi-stage build (esp32_sim + zeroclaw)
├── docker-compose.yml   ← simulator + agent services sharing /tmp
├── zeroclaw.toml.example ← constrained hardware-only config
├── .env.template        ← copy to .env
├── .gitignore
├── run-sim.sh           ← `docker compose up`
├── run-zeroclaw.sh      ← interactive agent inside container
└── run-daemon.sh        ← optional Docker daemon path, without Telegram
```

The simulator binary and visualizer live in:
`crates/zeroclaw-hardware/examples/esp32_sim.{rs,html}`

This demo harness depends on three focused changes extracted from the original
large contribution:
- dev-sim serial allowlist
- smartroom named-device tools (set_device / read_device)
- esp32_sim example + WebSocket frontend

See the individual PRs for those pieces.

## Troubleshooting

**`/tmp/zc-sim-esp32 not found`** — the simulator hasn't finished booting yet, or
socat failed. `docker compose logs` to see what happened.

**Agent replies in prose, doesn't call tools** — the system primer prompt above
needs to land before any user turn. If your model still won't call tools,
pre-flip pins via the manual buttons in the browser to keep the demo flowing.

**Container build fails on `cargo build`** — bump Docker Desktop's memory to 8GB+
in Settings → Resources. The hardware-feature build needs ~3GB peak.

**Agent doesn't see the smartroom tools** — make sure the mounted config has
`board = "esp32-sim"` (or `"esp32"`) under `[peripherals.boards]`. The smartroom
tools are only registered for those board types.

**Telegram shows tool approval prompts for normal chat** — check that
`risk_profiles.default.allowed_tools` only lists the smartroom tools. An empty
allowlist exposes every registered built-in tool to the model, including
session/admin tools that are not part of this demo.

## Quick start (recommended)

```bash
# 1. Copy and fill env (needed for the model)
cp demo/.env.template demo/.env
$EDITOR demo/.env

# 2. Start the simulator + visualizer
./demo/run-sim.sh

# 3. In another terminal, start an interactive agent session
./demo/run-zeroclaw.sh

# 4. Paste the system primer from demo/PROMPTS.md, then try natural language:
#    "It's getting dark and chilly. I'm settling in to read for an hour."
```

The browser visualizer is at http://127.0.0.1:8080.

## Notes

- All shell scripts in `demo/` are intentionally English-only (demo material).
- This harness exercises the full peripheral + dev-sim + smartroom path from the
  split PRs. It is not intended as a production template.
- For Telegram end-to-end testing, use `run-agent-host.sh`; it builds with the
  `channel-telegram` feature and runs the channel orchestrator directly.
