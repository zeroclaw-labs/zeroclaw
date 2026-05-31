# ZeroClaw ESP32 Smart Room Demo

Simulated ESP32 + ZeroClaw agent + browser visualization. Hardware-free.
Runs entirely inside a Docker container — nothing executes on the host.

## Architecture

```
[Host browser] ──:8080──┐
                        │
   ┌────────────────────▼─────────────────────────┐
   │  Docker container "zeroclaw-demo"            │
   │  ┌───────────────┐    ┌────────────────────┐ │
   │  │ esp32_sim     │    │ zeroclaw (chat)    │ │
   │  │ • socat       │    │ • MiniMax M2.7     │ │
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
web search, MCP, etc.) are disabled. The container provides defence in depth.

## Prerequisites

- Docker Desktop (tested on Docker 29 / Compose v2.40)
- A MiniMax API key (https://www.minimax.io/platform) **or** OpenRouter key

## Setup (one-time)

```bash
# 1. Configure secrets
cp demo/.env.template demo/.env
$EDITOR demo/.env       # paste MINIMAX_API_KEY=msk-...

# 2. Build the image (5-10 min on first build)
docker compose -f demo/docker-compose.yml build
```

## Run (two terminals)

**Terminal 1 — simulator + frontend:**
```bash
./demo/run-sim.sh
# or:  cd demo && docker compose up
```
Wait for `frontend ready: http://127.0.0.1:8080` then open that URL.

**Terminal 2 — interactive chat with the agent:**
```bash
./demo/run-zeroclaw.sh
# or:  cd demo && docker compose exec zeroclaw zeroclaw agent --config /app/zeroclaw.toml
```

When the chat prompt appears, paste the system primer:

> *"You control GPIO pins on a simulated ESP32 in a smart room. Pin map: 12=reading lamp, 13=overhead light, 14=heater, 2=fan/status LED, 5=motion sensor (input only). Use the gpio_write and gpio_read tools to actuate. Respond ONLY by calling tools — do not describe actions in prose. After all tool calls, write one sentence summarizing what you did. Acknowledge by reading the motion sensor first."*

Then ask things like:
- *"It's getting dark and chilly. I'm settling in to read for an hour."*
- *"Going to bed now."*
- *"Make it dramatic for a movie."*

The browser room SVG updates in real time as the agent flips pins.

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
└── run-daemon.sh        ← optional full daemon + Telegram path
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
- For the original Telegram end-to-end path, see `run-daemon.sh` and configure
  a channel in the mounted config (advanced).
