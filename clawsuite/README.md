# ClawSuite

### The Full-Stack Command Center for OpenClaw

**ClawSuite** is an open-source, self-hosted platform for OpenClaw AI agents. Not just a chat wrapper — it's a complete command center with built-in browser automation, skills marketplace, real-time dashboard, multi-agent orchestration, and enterprise-grade security scanning.

![ClawSuite Dashboard](./public/screenshots/dashboard.png)

> The first full-stack OpenClaw platform. Chat, browse, orchestrate, monitor — all in one place.

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Node Version](https://img.shields.io/badge/node-%3E%3D22.0.0-brightgreen.svg)](https://nodejs.org/)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-orange.svg)](CONTRIBUTING.md)

---

## ✨ Features

### 💬 **Intelligent Chat**

- Real-time conversations with AI agents powered by OpenClaw
- Multi-session management with session history
- Inline message editing and regeneration
- Markdown rendering with syntax highlighting
- Attachment support (images, files, code snippets)
- Message search (Cmd+F)

### 🤖 **Agent Hub**

- Browse and manage AI agent instances
- Launch CLI agents directly from the UI
- View active sessions and agent status
- Agent swarm orchestration for multi-agent workflows
- Real-time agent performance metrics

### 📊 **Dashboard & Monitoring**

- Live usage and cost tracking across all providers
- Interactive charts showing API consumption
- Provider-specific breakdowns (OpenAI, Anthropic, Google, etc.)
- Budget alerts and spending insights
- Gateway health monitoring

### 🌐 **Built-in Browser**

- Headed Chromium with stealth anti-detection
- Agent handoff — share pages directly with your AI
- Persistent sessions (login once, cookies survive restarts)
- Control panel for web automation tasks

### 🛒 **Skills Marketplace**

- Browse 2,000+ skills from ClawdHub registry
- One-click install with dependency resolution
- **Security scanning** — every skill scanned for suspicious patterns before install
- Auto-discovery of locally installed skills

### 🛠️ **Developer Tools**

- **Terminal**: Integrated terminal with cross-platform support
- **File Explorer**: Browse workspace files with Monaco code editor
- **Debug Console**: Gateway diagnostics with pattern-based troubleshooter
- **Memory Viewer**: Inspect and manage agent memory state
- **Cron Manager**: Schedule recurring tasks and automation

### 🔍 **Power User Features**

- **Global Search** (Cmd+K): Quick navigation across all screens
- **Browser Automation**: Control panel for web scraping and browser tasks
- **Activity Feed**: Real-time event stream from Gateway WebSocket
- **Session Management**: Pause, resume, or switch between conversations
- **Keyboard Shortcuts**: Press `?` to see all shortcuts

### 🎨 **Customization**

- Dynamic accent color system (pick any color)
- 3-way theme toggle (System / Light / Dark)
- Settings popup dialog with 6 tabs
- Provider setup wizard with guided onboarding
- Model switcher — always accessible, never disabled

### 🔒 **Security-First**

- Server-side API routes (keys never exposed to browser)
- Rate limiting on all endpoints
- Zod validation on all inputs
- Skills security scanning before install
- No hardcoded secrets in source

---

## 🚀 Getting Started

### Prerequisites

Before running ClawSuite, ensure you have:

- **Node.js 22+** ([Download](https://nodejs.org/))
- **OpenClaw Gateway** running locally ([Setup Guide](https://openclaw.ai/docs/installation))
  - Default gateway URL: `http://localhost:18789`
- **Python 3** (for integrated terminal PTY support)

### Quick Start

```bash
# 1. Clone the repository
git clone https://github.com/outsourc-e/clawsuite.git
cd clawsuite

# 2. Install dependencies
npm install

# 3. Install Playwright browser (required for Browser tab)
npx playwright install chromium

# 4. Set up environment variables
cp .env.example .env
# Edit .env with your gateway URL and token (see Environment Setup below)

# 5. Start development server
npm run dev
```

Open [http://localhost:3000](http://localhost:3000) in your browser.

### Environment Setup

ClawSuite connects to your local OpenClaw Gateway. Create a `.env` file (copy from `.env.example`) and configure:

**Required:**
- `CLAWDBOT_GATEWAY_URL` — WebSocket URL to your gateway
  - **Default:** `ws://127.0.0.1:18789` (local OpenClaw Gateway)
  - Use `ws://` for local, `wss://` for remote/encrypted connections

**Authentication (choose one):**
- `CLAWDBOT_GATEWAY_TOKEN` — **Recommended** authentication method
  - Find your token: Run `openclaw config get gateway.auth.token` in your terminal
  - Or check OpenClaw settings UI
  - Example format: `clw_abc123def456...` (64+ character token)
- `CLAWDBOT_GATEWAY_PASSWORD` — Alternative password-based auth

**Optional:**
- `CLAWSUITE_PASSWORD` — Protect ClawSuite with a password (leave empty for no auth)
- `CLAWSUITE_ALLOWED_HOSTS` — Allow access from non-localhost (e.g., Tailscale, LAN)
  - Example: `my-server.tail1234.ts.net,192.168.1.50`
  - Also binds server to `0.0.0.0` for network access

**First-time setup:**
ClawSuite will auto-detect a local gateway on first run if you leave the token blank. For production or remote gateways, authentication is required.

### Build for Production

```bash
# Build optimized production bundle
npm run build

# Preview production build
npm run preview
```

### Docker Setup

ClawSuite includes a `docker-compose.yml` for containerized deployment.

**Quick Start:**

```bash
# Build and run (detached)
docker compose up -d

# View logs
docker compose logs -f

# Stop
docker compose down
```

**Configuration:**

The container reads from the same `.env` file. Key differences:

- **Gateway URL:** Use `ws://host.docker.internal:18789` to reach the host machine's gateway
  - This is the default in `docker-compose.yml` if `CLAWDBOT_GATEWAY_URL` is not set
  - For remote gateways, use the actual hostname/IP
- **Port:** ClawSuite runs on `3000` inside the container, mapped to `3000` on the host
- **Network Access:** Set `CLAWSUITE_ALLOWED_HOSTS` to allow access from other machines

**Example `.env` for Docker:**

```bash
CLAWDBOT_GATEWAY_URL=ws://host.docker.internal:18789
CLAWDBOT_GATEWAY_TOKEN=clw_your_token_here
CLAWSUITE_ALLOWED_HOSTS=192.168.1.0/24
```

**Production Deployment:**

For production, consider:
- Using `wss://` with a reverse proxy (nginx, Caddy) for TLS
- Setting `CLAWSUITE_PASSWORD` to protect the interface
- Restricting `CLAWSUITE_ALLOWED_HOSTS` to specific IPs/domains

### Optional: Desktop App (Tauri)

ClawSuite can be packaged as a native desktop application using Tauri.

#### Ubuntu / Debian Prerequisites

Install the Rust toolchain and required system libraries before building:

```bash
# Install Rust via rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Install system dependencies
sudo apt update
sudo apt install -y \
  build-essential \
  pkg-config \
  libssl-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  librsvg2-dev
```

#### Run / Build

```bash
# Install Tauri CLI (if not already installed)
npm install -g @tauri-apps/cli

# Run desktop app
tauri dev

# Build desktop app
tauri build
```

---

## ⚙️ Configuration

### Runtime Settings

In addition to the `.env` file, you can adjust settings within the app:

1. **Settings → Gateway**:
   - View/update gateway URL
   - Test connection status
   - View authentication state

2. **Settings → Providers**:
   - Configure AI provider API keys (OpenAI, Anthropic, etc.)
   - These are stored in the gateway, not ClawSuite

3. **Settings → Appearance**:
   - Theme (System / Light / Dark)
   - Accent color
   - UI preferences

See [.env.example](.env.example) for all available environment variables.

---

## 🏗️ Architecture

ClawSuite is built with modern web technologies for performance and developer experience:

### Tech Stack

- **Framework**: [TanStack Start](https://tanstack.com/start) (React 19 SSR framework)
- **Routing**: [TanStack Router](https://tanstack.com/router) with file-based routing
- **State Management**: [TanStack Query](https://tanstack.com/query) + [Zustand](https://zustand-demo.pmnd.rs/)
- **Styling**: [Tailwind CSS 4](https://tailwindcss.com/)
- **Build Tool**: [Vite](https://vitejs.dev/)
- **Language**: TypeScript (strict mode)
- **Desktop**: [Tauri 2](https://tauri.app/) (optional)

### How It Works

ClawSuite acts as a **client UI** for the OpenClaw Gateway:

1. **Frontend** (React) renders the UI and handles user interactions
2. **Server Routes** (`/api/*`) proxy requests to the OpenClaw Gateway
3. **WebSocket** maintains real-time connection for streaming responses
4. **Gateway** processes AI requests and manages agent sessions

```
┌─────────────┐      HTTP/WebSocket      ┌──────────────┐
│  ClawSuite  │ ◄────────────────────────► │   Gateway    │
│  (Browser)  │                           │  (localhost)  │
└─────────────┘                           └──────────────┘
                                                  │
                                                  ▼
                                          ┌──────────────┐
                                          │  AI Providers │
                                          │ (OpenAI, etc) │
                                          └──────────────┘
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for detailed architecture documentation.

---

## 📁 Project Structure

```
clawsuite/
├── src/
│   ├── routes/           # TanStack Router routes + API endpoints
│   │   ├── index.tsx     # Dashboard (home page)
│   │   ├── chat/         # Chat interface
│   │   ├── terminal.tsx  # Integrated terminal
│   │   ├── skills.tsx    # Skills marketplace
│   │   ├── settings/     # Settings screens
│   │   └── api/          # Server-side API routes
│   │       ├── send.ts          # Send chat message
│   │       ├── stream.ts        # SSE streaming
│   │       ├── terminal-*.ts    # Terminal PTY
│   │       └── gateway/         # Gateway RPC proxy
│   ├── screens/          # Feature screen components
│   │   ├── chat/         # Chat UI logic
│   │   ├── dashboard/    # Dashboard widgets
│   │   ├── skills/       # Skills browser
│   │   └── settings/     # Settings panels
│   ├── components/       # Shared UI components
│   │   ├── ui/           # Base UI primitives
│   │   ├── terminal/     # Terminal components
│   │   ├── agent-chat/   # Chat message components
│   │   └── search/       # Global search (Cmd+K)
│   ├── lib/              # Utilities and helpers
│   │   ├── gateway-api.ts       # Gateway API client
│   │   ├── provider-catalog.ts  # AI provider metadata
│   │   └── utils.ts             # Shared utilities
│   ├── server/           # Server-side code
│   │   ├── gateway.ts           # Gateway RPC client
│   │   ├── terminal-sessions.ts # PTY session manager
│   │   └── pty-helper.py        # Python PTY wrapper
│   └── types/            # TypeScript type definitions
├── public/               # Static assets
├── docs/                 # Documentation
├── scripts/              # Build and dev scripts
└── src-tauri/            # Tauri desktop app config
```

---

## ⌨️ Keyboard Shortcuts

| Shortcut           | Action                  |
| ------------------ | ----------------------- |
| **Cmd+K** (Ctrl+K) | Open global search      |
| **Cmd+F** (Ctrl+F) | Search messages in chat |
| **Cmd+`** (Ctrl+`) | Toggle terminal         |
| **Cmd+Enter**      | Send message            |
| **Cmd+N**          | New chat session        |
| **Cmd+/**          | Toggle chat panel       |
| **?**              | Show all shortcuts      |
| **Esc**            | Close dialogs/modals    |

---

## 🤝 Contributing

We welcome contributions! Whether it's bug reports, feature requests, or code contributions, we'd love to hear from you.

### Quick Start

```bash
# Fork and clone the repo
git clone https://github.com/YOUR_USERNAME/clawsuite.git
cd clawsuite

# Create a feature branch
git checkout -b feature/your-feature-name

# Make your changes and commit
git commit -m "Add amazing feature"

# Push and open a PR
git push origin feature/your-feature-name
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines, including:

- Code style and conventions (TypeScript strict, Tailwind, **no portals/ScrollArea**)
- PR checklist and review process
- Architecture decisions
- Testing requirements

### Development Guidelines

- **No ScrollArea or Portal patterns**: Use native overflow and positioning
- **TypeScript strict mode**: All code must pass type checking
- **Tailwind-first**: Use utility classes; avoid custom CSS
- **Accessibility**: All interactive elements must be keyboard-navigable

---

## 📜 License

ClawSuite is open-source software licensed under the [MIT License](LICENSE).

---

## 🔗 Links

- **Website**: [clawsuite.io](https://clawsuite.io)
- **OpenClaw**: [openclaw.ai](https://openclaw.ai)
- **X (Twitter)**: [@clawsuite](https://x.com/clawsuite)
- **GitHub**: [outsourc-e/clawsuite](https://github.com/outsourc-e/clawsuite)
- **Documentation**: [docs/INDEX.md](docs/INDEX.md)

---

## 🙏 Acknowledgments

ClawSuite is built on top of the incredible [OpenClaw](https://openclaw.ai) project. Special thanks to all contributors and the open-source community.

---

## Extra

### Playwright Browser Setup

The **Browser tab** uses Playwright's Chromium binary for browser automation features:
- Headed browser with stealth anti-detection
- Direct page handoff to AI agents
- Persistent sessions (cookies survive restarts)
- Web scraping and automation tasks

**Installation:**

```bash
# Install Chromium binary (required for Browser tab)
npx playwright install chromium

# If you encounter missing system dependencies (common on fresh Ubuntu/Debian):
npx playwright install-deps chromium
```

**Note:** If you skip this step, clicking "Launch Browser" in the UI will fail silently. The install downloads ~350MB.

### Terminal Shell

The integrated terminal automatically detects your system shell via the `$SHELL` environment variable. If `$SHELL` is not set, it falls back to:

- **macOS**: `/bin/zsh`
- **Linux / Windows (WSL)**: `/bin/bash`

### Python 3 Requirement

The terminal uses a Python PTY helper for real pseudo-terminal support. Ensure `python3` is available on your `PATH`:

```bash
python3 --version
```

Most macOS and Linux systems include Python 3 by default. On minimal installations, install it with your package manager (e.g., `sudo apt install python3`).

---

**Built with 🦞 by [Eric](https://github.com/outsourc-e)**
