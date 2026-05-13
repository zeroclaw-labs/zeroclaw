<p align="center">
  <img src="https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/docs/assets/daemonclaw-banner.png" alt="DaemonClaw" width="600" />
</p>

<h1 align="center">🦀 DaemonClaw — Personal na AI Assistant</h1>

<p align="center">
  <strong>Zero overhead. Zero kompromiso. 100% Rust. 100% Agnostic.</strong><br>
  ⚡️ <strong>Tumatakbo sa $10 na hardware na may <5MB RAM: 99% mas kaunting memorya kaysa sa OpenClaw at 98% mas mura kaysa sa Mac mini!</strong>
</p>

<p align="center">
  <a href="https://github.com/DeliveryBoyTech/daemonclaw/actions/workflows/ci-run.yml"><img src="https://img.shields.io/github/actions/workflow/status/DeliveryBoyTech/daemonclaw/ci-run.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/DeliveryBoyTech/daemonclaw/releases/latest"><img src="https://img.shields.io/badge/version-v0.7.1-blue" alt="Version v0.7.1" /></a>
  <a href="https://github.com/DeliveryBoyTech/daemonclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/DeliveryBoyTech/daemonclaw?color=green" alt="Contributors" /></a>
  <a href="https://x.com/daemonclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40daemonclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @daemonclawlabs" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-Join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
  <a href="https://www.reddit.com/r/daemonclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fdaemonclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/daemonclawlabs" /></a>
</p>

<p align="center">
Binuo ng mga estudyante at miyembro ng mga komunidad ng Harvard, MIT, at Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Mga Wika:</strong>
  <a href="../../../README.md">🇺🇸 English</a> ·
  <a href="../zh-CN/README.md">🇨🇳 简体中文</a> ·
  <a href="../ja/README.md">🇯🇵 日本語</a> ·
  <a href="../ko/README.md">🇰🇷 한국어</a> ·
  <a href="../vi/README.md">🇻🇳 Tiếng Việt</a> ·
  <a href="../tl/README.md">🇵🇭 Tagalog</a> ·
  <a href="../es/README.md">🇪🇸 Español</a> ·
  <a href="../pt/README.md">🇧🇷 Português</a> ·
  <a href="../it/README.md">🇮🇹 Italiano</a> ·
  <a href="../de/README.md">🇩🇪 Deutsch</a> ·
  <a href="../fr/README.md">🇫🇷 Français</a> ·
  <a href="../ar/README.md">🇸🇦 العربية</a> ·
  <a href="../hi/README.md">🇮🇳 हिन्दी</a> ·
  <a href="../ru/README.md">🇷🇺 Русский</a> ·
  <a href="../bn/README.md">🇧🇩 বাংলা</a> ·
  <a href="../he/README.md">🇮🇱 עברית</a> ·
  <a href="../pl/README.md">🇵🇱 Polski</a> ·
  <a href="../cs/README.md">🇨🇿 Čeština</a> ·
  <a href="../nl/README.md">🇳🇱 Nederlands</a> ·
  <a href="../tr/README.md">🇹🇷 Türkçe</a> ·
  <a href="../uk/README.md">🇺🇦 Українська</a> ·
  <a href="../id/README.md">🇮🇩 Bahasa Indonesia</a> ·
  <a href="../th/README.md">🇹🇭 ไทย</a> ·
  <a href="../ur/README.md">🇵🇰 اردو</a> ·
  <a href="../ro/README.md">🇷🇴 Română</a> ·
  <a href="../sv/README.md">🇸🇪 Svenska</a> ·
  <a href="../el/README.md">🇬🇷 Ελληνικά</a> ·
  <a href="../hu/README.md">🇭🇺 Magyar</a> ·
  <a href="../fi/README.md">🇫🇮 Suomi</a> ·
  <a href="../da/README.md">🇩🇰 Dansk</a> ·
  <a href="../nb/README.md">🇳🇴 Norsk</a>
</p>

Ang DaemonClaw ay isang personal na AI assistant na pinapatakbo mo sa iyong sariling mga device. Sumasagot ito sa mga channel na ginagamit mo na (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, at marami pa). May web dashboard ito para sa real-time na kontrol at maaaring kumonekta sa hardware peripherals (ESP32, STM32, Arduino, Raspberry Pi). Ang Gateway ay control plane lamang — ang produkto ay ang assistant mismo.

Kung gusto mo ng personal, single-user na assistant na lokal, mabilis, at palaging naka-on, ito na iyon.

<p align="center">
  <a href="https://daemonclawlabs.ai">Website</a> ·
  <a href="docs/README.md">Docs</a> ·
  <a href="docs/architecture.md">Architecture</a> ·
  <a href="#mabilis-na-simula-tldr">Magsimula</a> ·
  <a href="#paglipat-mula-sa-openclaw">Paglipat mula sa OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Troubleshoot</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **Inirerekomendang setup:** patakbuhin ang `daemonclaw onboard` sa iyong terminal. Ang DaemonClaw Onboard ay gagabay sa iyo hakbang-hakbang sa pag-setup ng gateway, workspace, channel, at provider. Ito ang inirerekomendang setup path at gumagana sa macOS, Linux, at Windows (sa pamamagitan ng WSL2). Bagong install? Magsimula dito: [Magsimula](#mabilis-na-simula-tldr)

### Subscription Auth (OAuth)

- **OpenAI Codex** (subscription sa ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (API key o auth token)

Tala sa modelo: bagaman maraming provider/modelo ang sinusuportahan, para sa pinakamahusay na karanasan gamitin ang pinakamalakas na pinakabagong henerasyong modelo na available sa iyo. Tingnan ang [Onboarding](#mabilis-na-simula-tldr).

Configs ng modelo + CLI: [Providers reference](docs/reference/api/providers-reference.md)
Pag-rotate ng auth profile (OAuth vs API key) + failover: [Model failover](docs/reference/api/providers-reference.md)

## I-install (inirerekomenda)

Runtime: Rust stable toolchain. Isang binary lamang, walang runtime dependency.

### Homebrew (macOS/Linuxbrew)

```bash
brew install daemonclaw
```

### One-click bootstrap

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw
./install.sh
```

Awtomatikong tatakbo ang `daemonclaw onboard` pagkatapos ng install para i-configure ang iyong workspace at provider.

## Mabilis na Simula (TL;DR)

Kumpletong gabay para sa mga baguhan (auth, pairing, channels): [Magsimula](docs/setup-guides/one-click-bootstrap.md)

```bash
# Install + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Simulan ang gateway (webhook server + web dashboard)
daemonclaw gateway                # default: 127.0.0.1:42617
daemonclaw gateway --port 0       # random port (pinalakas na seguridad)

# Makipag-usap sa assistant
daemonclaw agent -m "Hello, DaemonClaw!"

# Interactive mode
daemonclaw agent

# Simulan ang buong autonomous runtime (gateway + channels + cron + hands)
daemonclaw daemon

# Tingnan ang status
daemonclaw status

# Patakbuhin ang diagnostics
daemonclaw doctor
```

Nag-upgrade? Patakbuhin ang `daemonclaw doctor` pagkatapos mag-update.

### Mula sa source (development)

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw

cargo build --release --locked
cargo install --path . --force --locked

daemonclaw onboard
```

> **Dev fallback (walang global install):** lagyan ng prefix ang mga command ng `cargo run --release --` (halimbawa: `cargo run --release -- status`).

## Paglipat mula sa OpenClaw

Maaaring i-import ng DaemonClaw ang iyong OpenClaw workspace, memory, at configuration:

```bash
# I-preview kung ano ang maili-lipat (ligtas, read-only)
daemonclaw migrate openclaw --dry-run

# Patakbuhin ang migration
daemonclaw migrate openclaw
```

Inililipat nito ang iyong memory entries, workspace files, at configuration mula `~/.openclaw/` patungo sa `~/.daemonclaw/`. Awtomatikong kino-convert ang config mula JSON patungong TOML.

## Mga default sa seguridad (DM access)

Kumokonekta ang DaemonClaw sa totoong mga messaging surface. Tratuhin ang mga papasok na DM bilang hindi mapagkakatiwalaang input.

Buong gabay sa seguridad: [SECURITY.md](SECURITY.md)

Default na gawi sa lahat ng channel:

- **DM pairing** (default): ang mga hindi kilalang nagpadala ay tumatanggap ng maikling pairing code at hindi pino-proseso ng bot ang kanilang mensahe.
- I-approve gamit ang: `daemonclaw pairing approve <channel> <code>` (pagkatapos ay idadagdag ang nagpadala sa lokal na allowlist).
- Ang mga pampublikong papasok na DM ay nangangailangan ng tahasang opt-in sa `config.toml`.
- Patakbuhin ang `daemonclaw doctor` para makita ang mga mapanganib o maling naka-configure na DM policy.

**Mga antas ng autonomy:**

| Antas | Gawi |
|-------|----------|
| `ReadOnly` | Maaari lamang magmasid ang agent, hindi kumilos |
| `Supervised` (default) | Kumikilos ang agent nang may pag-apruba para sa medium/high risk na operasyon |
| `Full` | Kumikilos ang agent nang autonomous sa loob ng mga hangganan ng patakaran |

**Mga layer ng sandboxing:** workspace isolation, path traversal blocking, command allowlisting, forbidden paths (`/etc`, `/root`, `~/.ssh`), rate limiting (max actions/hour, cost/day caps).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Mga Anunsyo

Gamitin ang talahanayan ito para sa mahahalagang paunawa (breaking changes, security advisories, maintenance windows, at release blockers).

| Petsa (UTC) | Antas | Paunawa | Aksyon |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Kritikal_ | **Hindi kami konektado** sa `openagen/daemonclaw`, `daemonclaw.org` o `daemonclaw.net`. Ang `daemonclaw.org` at `daemonclaw.net` na mga domain ay kasalukuyang nakaturo sa `openagen/daemonclaw` fork, at ang domain/repository na iyon ay nanggagaya sa aming opisyal na website/proyekto. | Huwag magtiwala sa impormasyon, binaries, fundraising, o mga anunsyo mula sa mga pinagmulang iyon. Gamitin lamang [ang repository na ito](https://github.com/DeliveryBoyTech/daemonclaw) at ang aming mga verified na social account. |
| 2026-02-19 | _Mahalaga_ | In-update ng Anthropic ang Authentication at Credential Use terms noong 2026-02-19. Ang Claude Code OAuth tokens (Free, Pro, Max) ay eksklusibong para sa Claude Code at Claude.ai; ang paggamit ng OAuth tokens mula sa Claude Free/Pro/Max sa anumang ibang produkto, tool, o serbisyo (kasama ang Agent SDK) ay hindi pinapahintulutan at maaaring lumabag sa Consumer Terms of Service. | Pansamantalang iwasan ang Claude Code OAuth integrations para maiwasan ang potensyal na pagkawala. Orihinal na clause: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## Mga Highlight

- **Magaan na Runtime bilang Default** — ang mga karaniwang CLI at status workflow ay tumatakbo sa loob ng ilang megabyte na memory envelope sa release builds.
- **Cost-Efficient na Deployment** — dinisenyo para sa $10 na board at maliliit na cloud instance, walang mabibigat na runtime dependency.
- **Mabilis na Cold Start** — single-binary Rust runtime na nagpapanatili ng halos instant na command at daemon startup.
- **Portable na Architecture** — isang binary sa buong ARM, x86, at RISC-V na may swappable na provider/channel/tool.
- **Local-first na Gateway** — iisang control plane para sa mga session, channel, tool, cron, SOP, at event.
- **Multi-channel na inbox** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket, at marami pa.
- **Multi-agent orchestration (Hands)** — mga autonomous na agent swarm na tumatakbo ayon sa iskedyul at nagiging mas matalino sa paglipas ng panahon.
- **Standard Operating Procedures (SOPs)** — event-driven workflow automation gamit ang MQTT, webhook, cron, at peripheral triggers.
- **Web Dashboard** — React 19 + Vite web UI na may real-time chat, memory browser, config editor, cron manager, at tool inspector.
- **Hardware peripherals** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO sa pamamagitan ng `Peripheral` trait.
- **First-class na mga tool** — shell, file I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace, at 70+ pa.
- **Lifecycle hooks** — i-intercept at baguhin ang mga LLM call, tool execution, at mensahe sa bawat yugto.
- **Skills platform** — bundled, community, at workspace skills na may security auditing.
- **Tunnel support** — Cloudflare, Tailscale, ngrok, OpenVPN, at custom tunnels para sa remote access.

### Bakit pinipili ng mga team ang DaemonClaw

- **Magaan bilang default:** maliit na Rust binary, mabilis na startup, mababang memory footprint.
- **Secure bilang disenyo:** pairing, strict sandboxing, explicit allowlists, workspace scoping.
- **Ganap na swappable:** ang mga core system ay traits (providers, channels, tools, memory, tunnels).
- **Walang lock-in:** OpenAI-compatible provider support + pluggable custom endpoints.

## Benchmark Snapshot (DaemonClaw vs OpenClaw, Reproducible)

Mabilis na benchmark sa lokal na machine (macOS arm64, Peb 2026) na normalized para sa 0.8GHz edge hardware.

|                           | OpenClaw      | NanoBot        | PicoClaw        | DaemonClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Wika**              | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Startup (0.8GHz core)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Laki ng Binary**           | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Gastos**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Kahit anong hardware $10** |

> Mga Tala: Ang mga resulta ng DaemonClaw ay sinusukat sa release builds gamit ang `/usr/bin/time -l`. Ang OpenClaw ay nangangailangan ng Node.js runtime (karaniwang ~390MB dagdag na memory overhead), habang ang NanoBot ay nangangailangan ng Python runtime. Ang PicoClaw at DaemonClaw ay static binaries. Ang mga RAM figure sa itaas ay runtime memory; ang build-time compilation requirements ay mas mataas.

<p align="center">
  <img src="docs/assets/daemonclaw-comparison.jpeg" alt="DaemonClaw vs OpenClaw Comparison" width="800" />
</p>

### Reproducible na lokal na pagsukat

```bash
cargo build --release
ls -lh target/release/daemonclaw

/usr/bin/time -l target/release/daemonclaw --help
/usr/bin/time -l target/release/daemonclaw status
```

## Lahat ng binuo namin

### Core platform

- Gateway HTTP/WS/SSE control plane na may mga session, presence, config, cron, webhooks, web dashboard, at pairing.
- CLI surface: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Agent orchestration loop na may tool dispatch, prompt construction, message classification, at memory loading.
- Session model na may security policy enforcement, autonomy levels, at approval gating.
- Resilient provider wrapper na may failover, retry, at model routing sa 20+ LLM backends.

### Mga Channel

Channel: WhatsApp (native), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Feature-gated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Web dashboard

React 19 + Vite 6 + Tailwind CSS 4 web dashboard na direktang inihahatid mula sa Gateway:

- **Dashboard** — pangkalahatang-tanaw ng sistema, health status, uptime, cost tracking
- **Agent Chat** — interactive chat kasama ang agent
- **Memory** — mag-browse at mag-manage ng memory entries
- **Config** — tingnan at i-edit ang configuration
- **Cron** — pamahalaan ang mga naka-schedule na gawain
- **Tools** — mag-browse ng mga available na tool
- **Logs** — tingnan ang mga agent activity log
- **Cost** — token usage at cost tracking
- **Doctor** — system health diagnostics
- **Integrations** — integration status at setup
- **Pairing** — device pairing management

### Mga firmware target

| Target | Platform | Layunin |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | Wireless peripheral agent |
| ESP32-UI | ESP32 + Display | Agent na may visual interface |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Industrial peripheral |
| Arduino | Arduino | Basic sensor/actuator bridge |
| Uno Q Bridge | Arduino Uno | Serial bridge patungo sa agent |

### Mga tool + automation

- **Core:** shell, file read/write/edit, git operations, glob search, content search
- **Web:** browser control, web fetch, web search, screenshot, image info, PDF read
- **Integrations:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + deferred tool sets
- **Scheduling:** cron add/remove/update/run, schedule tool
- **Memory:** recall, store, forget, knowledge, project intel
- **Advanced:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **Hardware:** board info, memory map, memory read (feature-gated)

### Runtime + kaligtasan

- **Mga antas ng autonomy:** ReadOnly, Supervised (default), Full.
- **Sandboxing:** workspace isolation, path traversal blocking, command allowlists, forbidden paths, Landlock (Linux), Bubblewrap.
- **Rate limiting:** max actions per hour, max cost per day (configurable).
- **Approval gating:** interactive approval para sa medium/high risk operations.
- **E-stop:** emergency shutdown capability.
- **129+ security tests** sa automated CI.

### Ops + packaging

- Web dashboard na direktang inihahatid mula sa Gateway.
- Tunnel support: Cloudflare, Tailscale, ngrok, OpenVPN, custom command.
- Docker runtime adapter para sa containerized execution.
- CI/CD: beta (auto sa push) → stable (manual dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Pre-built binaries para sa Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Configuration

Minimal na `~/.daemonclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Buong configuration reference: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Channel configuration

**Telegram:**
```toml
[channels.telegram]
bot_token = "123456:ABC-DEF..."
```

**Discord:**
```toml
[channels.discord]
token = "your-bot-token"
```

**Slack:**
```toml
[channels.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."
```

**WhatsApp:**
```toml
[channels.whatsapp]
enabled = true
```

**Matrix:**
```toml
[channels.matrix]
homeserver_url = "https://matrix.org"
username = "@bot:matrix.org"
password = "..."
```

**Signal:**
```toml
[channels.signal]
phone_number = "+1234567890"
```

### Tunnel configuration

```toml
[tunnel]
kind = "cloudflare"  # o "tailscale", "ngrok", "openvpn", "custom", "none"
```

Mga detalye: [Channel reference](docs/reference/api/channels-reference.md) · [Config reference](docs/reference/api/config-reference.md)

### Kasalukuyang runtime support

- **`native`** (default) — direct process execution, pinakamabilis na path, ideal para sa mga trusted environment.
- **`docker`** — buong container isolation, pinalakas na security policies, nangangailangan ng Docker.

Itakda ang `runtime.kind = "docker"` para sa strict sandboxing o network isolation.

## Subscription Auth (OpenAI Codex / Claude Code / Gemini)

Sinusuportahan ng DaemonClaw ang subscription-native auth profiles (multi-account, encrypted at rest).

- Store file: `~/.daemonclaw/auth-profiles.json`
- Encryption key: `~/.daemonclaw/.secret_key`
- Profile id format: `<provider>:<profile_name>` (halimbawa: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT subscription)
daemonclaw auth login --provider openai-codex --device-code

# Gemini OAuth
daemonclaw auth login --provider gemini --profile default

# Anthropic setup-token
daemonclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Tingnan / i-refresh / palitan ang profile
daemonclaw auth status
daemonclaw auth refresh --provider openai-codex --profile default
daemonclaw auth use --provider openai-codex --profile work

# Patakbuhin ang agent gamit ang subscription auth
daemonclaw agent --provider openai-codex -m "hello"
daemonclaw agent --provider anthropic -m "hello"
```

## Agent workspace + skills

Workspace root: `~/.daemonclaw/workspace/` (configurable sa pamamagitan ng config).

Mga injected prompt file:
- `IDENTITY.md` — personalidad at papel ng agent
- `USER.md` — konteksto at mga kagustuhan ng user
- `MEMORY.md` — pangmatagalang mga katotohanan at aral
- `AGENTS.md` — mga session convention at initialization rules
- `SOUL.md` — pangunahing pagkakakilanlan at mga operating principle

Skills: `~/.daemonclaw/workspace/skills/<skill>/SKILL.md` o `SKILL.toml`.

```bash
# Ilista ang mga naka-install na skill
daemonclaw skills list

# Mag-install mula sa git
daemonclaw skills install https://github.com/user/my-skill.git

# Security audit bago mag-install
daemonclaw skills audit https://github.com/user/my-skill.git

# Tanggalin ang isang skill
daemonclaw skills remove my-skill
```

## Mga CLI command

```bash
# Workspace management
daemonclaw onboard              # Guided setup wizard
daemonclaw status               # Ipakita ang daemon/agent status
daemonclaw doctor               # Patakbuhin ang system diagnostics

# Gateway + daemon
daemonclaw gateway              # Simulan ang gateway server (127.0.0.1:42617)
daemonclaw daemon               # Simulan ang buong autonomous runtime

# Agent
daemonclaw agent                # Interactive chat mode
daemonclaw agent -m "message"   # Single message mode

# Service management
daemonclaw service install      # I-install bilang OS service (launchd/systemd)
daemonclaw service start|stop|restart|status

# Mga channel
daemonclaw channel list         # Ilista ang mga configured na channel
daemonclaw channel doctor       # Suriin ang kalusugan ng channel
daemonclaw channel bind-telegram 123456789

# Cron + scheduling
daemonclaw cron list            # Ilista ang mga naka-schedule na gawain
daemonclaw cron add "*/5 * * * *" --prompt "Check system health"
daemonclaw cron remove <id>

# Memory
daemonclaw memory list          # Ilista ang mga memory entry
daemonclaw memory get <key>     # Kunin ang isang memory
daemonclaw memory stats         # Estadistika ng memory

# Auth profiles
daemonclaw auth login --provider <name>
daemonclaw auth status
daemonclaw auth use --provider <name> --profile <profile>

# Hardware peripherals
daemonclaw hardware discover    # I-scan ang mga konektadong device
daemonclaw peripheral list      # Ilista ang mga konektadong peripheral
daemonclaw peripheral flash     # I-flash ang firmware sa device

# Migration
daemonclaw migrate openclaw --dry-run
daemonclaw migrate openclaw

# Shell completions
source <(daemonclaw completions bash)
daemonclaw completions zsh > ~/.zfunc/_daemonclaw
```

Buong commands reference: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Mga Kinakailangan

<details>
<summary><strong>Windows</strong></summary>

#### Kinakailangan

1. **Visual Studio Build Tools** (nagbibigay ng MSVC linker at Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Sa panahon ng installation (o sa pamamagitan ng Visual Studio Installer), piliin ang **"Desktop development with C++"** workload.

2. **Rust toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Pagkatapos ng installation, magbukas ng bagong terminal at patakbuhin ang `rustup default stable` para matiyak na aktibo ang stable toolchain.

3. **I-verify** na pareho ay gumagana:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opsyonal

- **Docker Desktop** — kinakailangan lamang kung gumagamit ng [Docker sandboxed runtime](#kasalukuyang-runtime-support) (`runtime.kind = "docker"`). I-install sa pamamagitan ng `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Kinakailangan

1. **Build essentials:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** I-install ang Xcode Command Line Tools: `xcode-select --install`

2. **Rust toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Tingnan ang [rustup.rs](https://rustup.rs) para sa mga detalye.

3. **I-verify** na pareho ay gumagana:
    ```bash
    rustc --version
    cargo --version
    ```

#### One-Line Installer

O laktawan ang mga hakbang sa itaas at i-install ang lahat (system deps, Rust, DaemonClaw) sa isang command:

```bash
curl -LsSf https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/install.sh | bash
```

#### Mga kinakailangan sa compilation resources

Ang pagbuo mula sa source ay nangangailangan ng mas maraming resources kaysa sa pagpapatakbo ng resultang binary:

| Resource | Minimum | Inirerekomenda |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Libreng disk**  | 6 GB    | 10 GB+      |

Kung ang iyong host ay nasa ibaba ng minimum, gumamit ng pre-built binaries:

```bash
./install.sh --prefer-prebuilt
```

Para sa binary-only install na walang source fallback:

```bash
./install.sh --prebuilt-only
```

#### Opsyonal

- **Docker** — kinakailangan lamang kung gumagamit ng [Docker sandboxed runtime](#kasalukuyang-runtime-support) (`runtime.kind = "docker"`). I-install sa pamamagitan ng iyong package manager o [docker.com](https://docs.docker.com/engine/install/).

> **Tala:** Ang default na `cargo build --release` ay gumagamit ng `codegen-units=1` para mabawasan ang peak compile pressure. Para sa mas mabilis na build sa mga powerful machine, gamitin ang `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Mga pre-built binary

Ang mga release asset ay nai-publish para sa:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

I-download ang pinakabagong asset mula sa:
<https://github.com/DeliveryBoyTech/daemonclaw/releases/latest>

## Docs

Gamitin ang mga ito kapag tapos ka na sa onboarding flow at gusto mo ng mas malalim na reference.

- Magsimula sa [docs index](docs/README.md) para sa navigation at "ano ang nasaan."
- Basahin ang [architecture overview](docs/architecture.md) para sa buong system model.
- Gamitin ang [configuration reference](docs/reference/api/config-reference.md) kapag kailangan mo ng bawat key at halimbawa.
- Patakbuhin ang Gateway ayon sa [operational runbook](docs/ops/operations-runbook.md).
- Sundin ang [DaemonClaw Onboard](#mabilis-na-simula-tldr) para sa guided setup.
- I-debug ang mga karaniwang pagkabigo gamit ang [troubleshooting guide](docs/ops/troubleshooting.md).
- Suriin ang [security guidance](docs/security/README.md) bago i-expose ang kahit ano.

### Mga reference doc

- Documentation hub: [docs/README.md](docs/README.md)
- Unified docs TOC: [docs/SUMMARY.md](docs/SUMMARY.md)
- Commands reference: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Config reference: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Providers reference: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Channels reference: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Operations runbook: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Troubleshooting: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Mga collaboration doc

- Contribution guide: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR workflow policy: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI workflow guide: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Reviewer playbook: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Security disclosure policy: [SECURITY.md](SECURITY.md)
- Documentation template: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Deployment + operations

- Network deployment guide: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Proxy agent playbook: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Hardware guides: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

Ang DaemonClaw ay binuo para sa smooth crab 🦀, isang mabilis at mahusay na AI assistant. Binuo ni Argenis De La Rosa at ng komunidad.

- [daemonclawlabs.ai](https://daemonclawlabs.ai)
- [@daemonclawlabs](https://x.com/daemonclawlabs)

## Suportahan ang DaemonClaw

### 🙏 Espesyal na Pasasalamat

Isang taos-pusong pasasalamat sa mga komunidad at institusyon na nagbibigay-inspirasyon at nagpapaganap sa open-source work na ito:

- **Harvard University** — para sa pagpapaunlad ng intelektwal na kuryosidad at pagtulak sa mga hangganan ng kung ano ang posible.
- **MIT** — para sa pagtataguyod ng bukas na kaalaman, open source, at ang paniniwala na ang teknolohiya ay dapat na naa-access ng lahat.
- **Sundai Club** — para sa komunidad, enerhiya, at ang walang pagod na pagnanais na bumuo ng mga bagay na mahalaga.
- **Ang Mundo at Higit Pa** 🌍✨ — sa bawat contributor, panaginip, at builder na gumagawa ng open source bilang puwersa para sa kabutihan. Ito ay para sa inyo.

Bumubuo kami ng bukas dahil ang mga pinakamahusay na ideya ay nanggagaling sa lahat ng dako. Kung binabasa mo ito, bahagi ka nito. Maligayang pagdating. 🦀❤️

## Mag-contribute

Bago sa DaemonClaw? Hanapin ang mga issue na may label na [`good first issue`](https://github.com/DeliveryBoyTech/daemonclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — tingnan ang aming [Contributing Guide](CONTRIBUTING.md#first-time-contributors) kung paano magsimula. Ang AI/vibe-coded PRs ay welcome! 🤖

Tingnan ang [CONTRIBUTING.md](CONTRIBUTING.md) at [CLA.md](docs/contributing/cla.md). Mag-implement ng trait, mag-submit ng PR:

- CI workflow guide: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Bagong `Provider` → `src/providers/`
- Bagong `Channel` → `src/channels/`
- Bagong `Observer` → `src/observability/`
- Bagong `Tool` → `src/tools/`
- Bagong `Memory` → `src/memory/`
- Bagong `Tunnel` → `src/tunnel/`
- Bagong `Peripheral` → `src/peripherals/`
- Bagong `Skill` → `~/.daemonclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Opisyal na Repository at Babala sa Panggagaya

**Ito ang tanging opisyal na DaemonClaw repository:**

> https://github.com/DeliveryBoyTech/daemonclaw

Ang anumang iba pang repository, organisasyon, domain, o package na nag-aangkin na "DaemonClaw" o nagpapahiwatig ng affiliation sa DaemonClaw Labs ay **hindi awtorisado at hindi konektado sa proyektong ito**. Ang mga kilalang unauthorized forks ay ililista sa [TRADEMARK.md](docs/maintainers/trademark.md).

Kung makakita ka ng panggagaya o trademark misuse, mangyaring [mag-open ng issue](https://github.com/DeliveryBoyTech/daemonclaw/issues).

---

## Lisensya

Ang DaemonClaw ay dual-licensed para sa maximum na openness at proteksyon ng contributor:

| Lisensya | Gamit |
|---|---|
| [MIT](LICENSE-MIT) | Open-source, pananaliksik, akademiko, personal na gamit |
| [Apache 2.0](LICENSE-APACHE) | Patent protection, institutional, commercial deployment |

Maaari kang pumili ng alinmang lisensya. **Awtomatikong nagbibigay ang mga contributor ng karapatan sa ilalim ng pareho** — tingnan ang [CLA.md](docs/contributing/cla.md) para sa buong contributor agreement.

### Trademark

Ang pangalang **DaemonClaw** at logo ay mga trademark ng DaemonClaw Labs. Ang lisensyang ito ay hindi nagbibigay ng pahintulot na gamitin ang mga ito upang ipahiwatig ang endorsement o affiliation. Tingnan ang [TRADEMARK.md](docs/maintainers/trademark.md) para sa mga pinapahintulutan at ipinagbabawal na gamit.

### Mga Proteksyon ng Contributor

- **Pinapanatili mo ang copyright** ng iyong mga kontribusyon
- **Patent grant** (Apache 2.0) ay nagpoprotekta sa iyo mula sa patent claims ng ibang mga contributor
- Ang iyong mga kontribusyon ay **permanenteng naka-attribute** sa commit history at [NOTICE](NOTICE)
- Walang trademark rights ang naililipat sa pamamagitan ng pag-contribute

---

**DaemonClaw** — Zero overhead. Zero kompromiso. I-deploy kahit saan. I-swap ang kahit ano. 🦀

## Mga Contributor

<a href="https://github.com/DeliveryBoyTech/daemonclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=DeliveryBoyTech/daemonclaw" alt="DaemonClaw contributors" />
</a>

Ang listahang ito ay generated mula sa GitHub contributors graph at awtomatikong nag-a-update.

## Star History

<p align="center">
  <a href="https://www.star-history.com/#DeliveryBoyTech/daemonclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
