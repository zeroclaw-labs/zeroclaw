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

## Low-storage / MacBook Air development path (recommended for limited disk)

If you cannot allocate 60-80+ GB to Docker Desktop, test the vignette directly on your host instead. This is much lighter and lets you iterate quickly.

```bash
# 1. One-time setup
cp demo/.env.template demo/.env
nano demo/.env          # easiest on Mac. Use: code demo/.env  or  vim demo/.env
                        # At minimum, set:  MINIMAX_API_KEY="your-real-key"

# 2. Install socat if missing (required by the simulator)
brew install socat

# 3. Ensure demo config exists
mkdir -p demo/data/config
cp -n demo/zeroclaw.toml.example demo/data/config/config.toml || true

# 4. Terminal 1 – start simulator + visualizer
./demo/run-sim-host.sh

# Wait for "frontend ready: http://127.0.0.1:8080", then open the URL.

# 5. Terminal 2 – start the agent
./demo/run-agent-host.sh
```

Then paste the system primer from `demo/PROMPTS.md` and use natural language exactly as in the Docker path.

This gives you the full functional vignette (smartroom tools → pty → simulator → live SVG) with far less disk pressure.

## Packaged demo (Docker) – requires decent disk

Use this path when you want the one-command "everything in a container" experience for demos or sharing.

**Requirements**
- Docker Desktop with **at least 60-80 GB** allocated to the disk image (see Settings → Resources)
- A MiniMax or OpenRouter key

**One-time setup**

```bash
cp demo/.env.template demo/.env
$EDITOR demo/.env
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
