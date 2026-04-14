<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — Personlig AI-assistent</h1>

<p align="center">
  <strong>Noll overhead. Noll kompromiss. 100% Rust. 100% Agnostisk.</strong><br>
  ⚡️ <strong>Körs på $10-hårdvara med <5MB RAM: Det är 99% mindre minne än OpenClaw och 98% billigare än en Mac mini!</strong>
</p>

<p align="center">
Byggt av studenter och medlemmar i Harvard-, MIT- och Sundai.Club-gemenskaperna.
</p>

<p align="center">
  🌐 <strong>Språk:</strong>
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

QuantClaw är en personlig AI-assistent som du kör på dina egna enheter. Den svarar dig via de kanaler du redan använder (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work med flera). Den har en webbpanel för realtidskontroll och kan ansluta till hårdvaruperiferienheter (ESP32, STM32, Arduino, Raspberry Pi). Gateway är bara kontrollplanet — produkten är assistenten.

Om du vill ha en personlig, enanvändarassistent som känns lokal, snabb och alltid tillgänglig, är det här lösningen.

<p align="center">
  <a href="https://quantspeed.ai">Webbplats</a> ·
  <a href="docs/README.md">Dokumentation</a> ·
  <a href="docs/architecture.md">Arkitektur</a> ·
  <a href="#snabbstart">Kom igång</a> ·
  <a href="#migrera-från-openclaw">Migrera från OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Felsökning</a> ·
</p>

> **Rekommenderad konfiguration:** kör `quantclaw onboard` i din terminal. QuantClaw Onboard guidar dig steg för steg genom att konfigurera gateway, arbetsyta, kanaler och leverantör. Det är den rekommenderade installationsvägen och fungerar på macOS, Linux och Windows (via WSL2). Ny installation? Börja här: [Kom igång](#snabbstart)

### Prenumerationsautentisering (OAuth)

- **OpenAI Codex** (ChatGPT-prenumeration)
- **Gemini** (Google OAuth)
- **Anthropic** (API-nyckel eller autentiseringstoken)

Modellnotering: även om många leverantörer/modeller stöds, använd den starkaste senaste generationens modell som är tillgänglig för dig för bästa upplevelse. Se [Onboarding](#snabbstart).

Modellkonfiguration + CLI: [Leverantörsreferens](docs/reference/api/providers-reference.md)
Autentiseringsprofil-rotation (OAuth vs API-nycklar) + failover: [Modell-failover](docs/reference/api/providers-reference.md)

## Installation (rekommenderad)

Körmiljö: Rust stable toolchain. Enda binär, inga körtidsberoenden.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### Ett-klicks-installation

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` körs automatiskt efter installationen för att konfigurera din arbetsyta och leverantör.

## Snabbstart

Fullständig nybörjarguide (autentisering, parkoppling, kanaler): [Kom igång](docs/setup-guides/one-click-bootstrap.md)

```bash
# Installera + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Starta gateway (webhook-server + webbpanel)
quantclaw gateway                # standard: 127.0.0.1:42617
quantclaw gateway --port 0       # slumpmässig port (säkerhetshärdad)

# Prata med assistenten
quantclaw agent -m "Hello, QuantClaw!"

# Interaktivt läge
quantclaw agent

# Starta full autonom körmiljö (gateway + kanaler + cron + hands)
quantclaw daemon

# Kontrollera status
quantclaw status

# Kör diagnostik
quantclaw doctor
```

Uppgraderar du? Kör `quantclaw doctor` efter uppdatering.

### Från källkod (utveckling)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **Utvecklar-fallback (ingen global installation):** prefixera kommandon med `cargo run --release --` (exempel: `cargo run --release -- status`).

## Migrera från OpenClaw

QuantClaw kan importera din OpenClaw-arbetsyta, minne och konfiguration:

```bash
# Förhandsgranska vad som migreras (säkert, skrivskyddat)
quantclaw migrate openclaw --dry-run

# Kör migreringen
quantclaw migrate openclaw
```

Detta migrerar dina minnesposter, arbetsytefiler och konfiguration från `~/.openclaw/` till `~/.quantclaw/`. Konfiguration konverteras automatiskt från JSON till TOML.

## Säkerhetsstandarder (DM-åtkomst)

QuantClaw ansluter till riktiga meddelandeytor. Behandla inkommande DM som opålitlig indata.

Fullständig säkerhetsguide: [SECURITY.md](SECURITY.md)

Standardbeteende på alla kanaler:

- **DM-parkoppling** (standard): okända avsändare får en kort parkopplingskod och boten behandlar inte deras meddelande.
- Godkänn med: `quantclaw pairing approve <channel> <code>` (sedan läggs avsändaren till i en lokal tillåtlista).
- Offentliga inkommande DM kräver ett explicit opt-in i `config.toml`.
- Kör `quantclaw doctor` för att hitta riskfyllda eller felkonfigurerade DM-policyer.

**Autonominivåer:**

| Nivå | Beteende |
|------|----------|
| `ReadOnly` | Agenten kan observera men inte agera |
| `Supervised` (standard) | Agenten agerar med godkännande för medel-/högriskoperationer |
| `Full` | Agenten agerar autonomt inom policygränser |

**Sandboxlager:** arbetsyteisolering, sökvägstraversblockering, kommandotillåtlistor, förbjudna sökvägar (`/etc`, `/root`, `~/.ssh`), hastighetsbegränsning (max åtgärder/timme, kostnad/dag-gränser).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Meddelanden

Använd denna tavla för viktiga meddelanden (brytande ändringar, säkerhetsrådgivningar, underhållsfönster och releaseblockerare).

| Datum (UTC) | Nivå        | Meddelande                                                                                                                                                                                                                                                                                                                                             | Åtgärd                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ----------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19  | _Kritisk_   | Vi är **inte affilierade** med `openagen/quantclaw`, `quantclaw.org` eller `quantclaw.net`. Domänerna `quantclaw.org` och `quantclaw.net` pekar för närvarande till `openagen/quantclaw`-forken, och den domänen/repositoryt utger sig för att vara vår officiella webbplats/projekt.                                                                         | Lita inte på information, binärer, insamlingar eller meddelanden från dessa källor. Använd bara [detta repository](https://github.com/quant-speed/quantclaw) och våra verifierade sociala konton.                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 2026-02-19  | _Viktigt_   | Anthropic uppdaterade villkoren för autentisering och inloggningsanvändning 2026-02-19. Claude Code OAuth-tokens (Free, Pro, Max) är avsedda uteslutande för Claude Code och Claude.ai; att använda OAuth-tokens från Claude Free/Pro/Max i någon annan produkt, verktyg eller tjänst (inklusive Agent SDK) är inte tillåtet och kan bryta mot Consumer Terms of Service. | Undvik tillfälligt Claude Code OAuth-integrationer för att förhindra potentiell förlust. Originalklausul: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                                              |

## Höjdpunkter

- **Lean körmiljö som standard** — vanliga CLI- och statusarbetsflöden körs i ett fåmegabyte-minnesutrymme på release-byggen.
- **Kostnadseffektiv distribution** — designad för $10-kort och små molninstanser, inga tunga körtidsberoenden.
- **Snabba kallstarter** — enkel binär Rust-körmiljö håller kommando- och daemon-uppstart nära ögonblicklig.
- **Portabel arkitektur** — en binär över ARM, x86 och RISC-V med utbytbara providers/channels/tools.
- **Lokal-först Gateway** — enda kontrollplan för sessioner, kanaler, verktyg, cron, SOP:er och händelser.
- **Multikanalinkorg** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket med flera.
- **Multiagentorkestrering (Hands)** — autonoma agentsvärmar som körs på schema och blir smartare med tiden.
- **Standardoperationsprocedurer (SOPs)** — händelsedriven arbetsflödesautomatisering med MQTT, webhook, cron och periferiutlösare.
- **Webbpanel** — React 19 + Vite webb-UI med realtidschatt, minnesutforskare, konfigurationsredigerare, cron-hanterare och verktygsinspektor.
- **Hårdvaruperiferienheter** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO via `Peripheral`-traiten.
- **Förstklassiga verktyg** — shell, fil-I/O, webbläsare, git, web fetch/search, MCP, Jira, Notion, Google Workspace och 70+ fler.
- **Livscykelkrokar** — fånga upp och modifiera LLM-anrop, verktygsexekveringar och meddelanden i varje steg.
- **Färdighetsplattform** — medföljande, community- och arbetsytefärdigheter med säkerhetsgranskning.
- **Tunnelstöd** — Cloudflare, Tailscale, ngrok, OpenVPN och anpassade tunnlar för fjärråtkomst.

### Varför team väljer QuantClaw

- **Lean som standard:** liten Rust-binär, snabb start, lågt minnesavtryck.
- **Säker från grunden:** parkoppling, strikt sandboxning, explicita tillåtlistor, arbetsyteavgränsning.
- **Fullt utbytbar:** kärnssystem är traits (providers, channels, tools, memory, tunnels).
- **Inget leverantörslås:** OpenAI-kompatibelt leverantörsstöd + pluggbara anpassade endpoints.

## Benchmarkögonblicksbild (QuantClaw vs OpenClaw, Reproducerbar)

Lokal maskin-snabbtest (macOS arm64, feb 2026) normaliserat för 0.8GHz edge-hårdvara.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Språk**                 | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Uppstart (0.8GHz kärna)** | > 500s      | > 30s          | < 1s            | **< 10ms**           |
| **Binärstorlek**          | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Kostnad**               | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Vilken hårdvara som helst $10** |

> Noteringar: QuantClaw-resultat mäts på release-byggen med `/usr/bin/time -l`. OpenClaw kräver Node.js-körmiljö (typiskt ~390MB extra minnesoverhead), medan NanoBot kräver Python-körmiljö. PicoClaw och QuantClaw är statiska binärer. RAM-siffrorna ovan är körtidsminne; kompileringskrav vid byggtid är högre.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw jämförelse" width="800" />
</p>

### Reproducerbar lokal mätning

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## Allt vi byggt hittills

### Kärnplattform

- Gateway HTTP/WS/SSE-kontrollplan med sessioner, närvaro, konfiguration, cron, webhooks, webbpanel och parkoppling.
- CLI-yta: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Agentorkestreringsloop med verktygsdistribution, promptkonstruktion, meddelandeklassificering och minnesinläsning.
- Sessionsmodell med säkerhetspolicyefterlevnad, autonominivåer och godkännandeportar.
- Motståndskraftig leverantörswrapper med failover, retry och modellroutning över 20+ LLM-backends.

### Kanaler

Kanaler: WhatsApp (nativ), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Funktionsgated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Webbpanel

React 19 + Vite 6 + Tailwind CSS 4 webbpanel serverad direkt från Gateway:

- **Dashboard** — systemöversikt, hälsostatus, drifttid, kostnadsspårning
- **Agentchatt** — interaktiv chatt med agenten
- **Minne** — bläddra och hantera minnesposter
- **Konfiguration** — visa och redigera konfiguration
- **Cron** — hantera schemalagda uppgifter
- **Verktyg** — bläddra tillgängliga verktyg
- **Loggar** — visa agentaktivitetsloggar
- **Kostnad** — tokenanvändning och kostnadsspårning
- **Doktor** — systemhälsodiagnostik
- **Integrationer** — integrationsstatus och konfiguration
- **Parkoppling** — hantering av enhetsparkoppling

### Firmware-mål

| Mål | Plattform | Syfte |
|-----|-----------|-------|
| ESP32 | Espressif ESP32 | Trådlös periferienhetagent |
| ESP32-UI | ESP32 + Display | Agent med visuellt gränssnitt |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Industriell periferienhet |
| Arduino | Arduino | Grundläggande sensor-/aktuatorbrygga |
| Uno Q Bridge | Arduino Uno | Seriell brygga till agent |

### Verktyg + automatisering

- **Kärna:** shell, filläsning/skrivning/redigering, git-operationer, glob-sökning, innehållssökning
- **Webb:** webbläsarkontroll, web fetch, webbsökning, skärmdump, bildinformation, PDF-läsning
- **Integrationer:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol-verktygs-wrapper + uppskjutna verktygsuppsättningar
- **Schemaläggning:** cron add/remove/update/run, schemaverktyg
- **Minne:** recall, store, forget, knowledge, project intel
- **Avancerat:** delegate (agent-till-agent), swarm, modellväxling/routing, säkerhetsoperationer, molnoperationer
- **Hårdvara:** board info, memory map, memory read (funktionsgated)

### Körmiljö + säkerhet

- **Autonominivåer:** ReadOnly, Supervised (standard), Full.
- **Sandboxning:** arbetsyteisolering, sökvägstraversblockering, kommandotillåtlistor, förbjudna sökvägar, Landlock (Linux), Bubblewrap.
- **Hastighetsbegränsning:** max åtgärder per timme, max kostnad per dag (konfigurerbart).
- **Godkännandeportar:** interaktivt godkännande för medel-/högriskoperationer.
- **E-stopp:** nödavstängningskapacitet.
- **129+ säkerhetstester** i automatiserad CI.

### Drift + paketering

- Webbpanel serverad direkt från Gateway.
- Tunnelstöd: Cloudflare, Tailscale, ngrok, OpenVPN, anpassat kommando.
- Docker-körmiljöadapter för containeriserad exekvering.
- CI/CD: beta (automatiskt vid push) → stable (manuell dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Förbyggda binärer för Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Konfiguration

Minimal `~/.quantclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Fullständig konfigurationsreferens: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Kanalkonfiguration

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

### Tunnelkonfiguration

```toml
[tunnel]
kind = "cloudflare"  # eller "tailscale", "ngrok", "openvpn", "custom", "none"
```

Detaljer: [Kanalreferens](docs/reference/api/channels-reference.md) · [Konfigurationsreferens](docs/reference/api/config-reference.md)

### Körmiljöstöd (nuvarande)

- **`native`** (standard) — direkt processexekvering, snabbaste vägen, idealisk för betrodda miljöer.
- **`docker`** — full containerisolering, tvingade säkerhetspolicyer, kräver Docker.

Ställ in `runtime.kind = "docker"` för strikt sandboxning eller nätverksisolering.

## Prenumerationsautentisering (OpenAI Codex / Claude Code / Gemini)

QuantClaw stöder prenumerationsnativa autentiseringsprofiler (multikonto, krypterat i vila).

- Lagringsfil: `~/.quantclaw/auth-profiles.json`
- Krypteringsnyckel: `~/.quantclaw/.secret_key`
- Profil-ID-format: `<provider>:<profile_name>` (exempel: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT-prenumeration)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Kontrollera / uppdatera / byt profil
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Kör agenten med prenumerationsautentisering
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## Agentarbetsyta + färdigheter

Arbetsyterot: `~/.quantclaw/workspace/` (konfigurerbart via config).

Injicerade promptfiler:
- `IDENTITY.md` — agentpersonlighet och roll
- `USER.md` — användarkontext och preferenser
- `MEMORY.md` — långtidsfakta och lärdomar
- `AGENTS.md` — sessionskonventioner och initieringsregler
- `SOUL.md` — kärnidentitet och operationsprinciper

Färdigheter: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` eller `SKILL.toml`.

```bash
# Lista installerade färdigheter
quantclaw skills list

# Installera från git
quantclaw skills install https://github.com/user/my-skill.git

# Säkerhetsgranskning före installation
quantclaw skills audit https://github.com/user/my-skill.git

# Ta bort en färdighet
quantclaw skills remove my-skill
```

## CLI-kommandon

```bash
# Arbetsytehantering
quantclaw onboard              # Guidad installationsguide
quantclaw status               # Visa daemon-/agentstatus
quantclaw doctor               # Kör systemdiagnostik

# Gateway + daemon
quantclaw gateway              # Starta gateway-server (127.0.0.1:42617)
quantclaw daemon               # Starta full autonom körmiljö

# Agent
quantclaw agent                # Interaktivt chattläge
quantclaw agent -m "message"   # Enstaka meddelandeläge

# Tjänstehantering
quantclaw service install      # Installera som OS-tjänst (launchd/systemd)
quantclaw service start|stop|restart|status

# Kanaler
quantclaw channel list         # Lista konfigurerade kanaler
quantclaw channel doctor       # Kontrollera kanalhälsa
quantclaw channel bind-telegram 123456789

# Cron + schemaläggning
quantclaw cron list            # Lista schemalagda jobb
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Minne
quantclaw memory list          # Lista minnesposter
quantclaw memory get <key>     # Hämta ett minne
quantclaw memory stats         # Minnesstatistik

# Autentiseringsprofiler
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Hårdvaruperiferienheter
quantclaw hardware discover    # Sök efter anslutna enheter
quantclaw peripheral list      # Lista anslutna periferienheter
quantclaw peripheral flash     # Flasha firmware till enhet

# Migrering
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Shell-kompletteringar
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

Fullständig kommandoreferens: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Förutsättningar

<details>
<summary><strong>Windows</strong></summary>

#### Obligatoriskt

1. **Visual Studio Build Tools** (tillhandahåller MSVC-länkaren och Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Under installationen (eller via Visual Studio Installer), välj arbetsbelastningen **"Desktop development with C++"**.

2. **Rust toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Efter installationen, öppna en ny terminal och kör `rustup default stable` för att säkerställa att stable-toolchainen är aktiv.

3. **Verifiera** att båda fungerar:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Valfritt

- **Docker Desktop** — krävs bara om du använder [Docker sandboxad körmiljö](#körmiljöstöd-nuvarande) (`runtime.kind = "docker"`). Installera via `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Obligatoriskt

1. **Byggverktyg:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Installera Xcode Command Line Tools: `xcode-select --install`

2. **Rust toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Se [rustup.rs](https://rustup.rs) för detaljer.

3. **Verifiera** att båda fungerar:
    ```bash
    rustc --version
    cargo --version
    ```

#### Enradsinstallerare

Eller hoppa över stegen ovan och installera allt (systemberoenden, Rust, QuantClaw) med ett enda kommando:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### Kompileringsresurskrav

Att bygga från källkod kräver mer resurser än att köra den resulterande binären:

| Resurs         | Minimum | Rekommenderat |
| -------------- | ------- | ------------- |
| **RAM + swap** | 2 GB    | 4 GB+         |
| **Ledigt disk**| 6 GB    | 10 GB+        |

Om din värd ligger under minimum, använd förbyggda binärer:

```bash
./install.sh --prefer-prebuilt
```

För att kräva enbart binärinstallation utan källkods-fallback:

```bash
./install.sh --prebuilt-only
```

#### Valfritt

- **Docker** — krävs bara om du använder [Docker sandboxad körmiljö](#körmiljöstöd-nuvarande) (`runtime.kind = "docker"`). Installera via din pakethanterare eller [docker.com](https://docs.docker.com/engine/install/).

> **Notering:** Standard `cargo build --release` använder `codegen-units=1` för att minska toppkompileringstrycket. För snabbare byggen på kraftfulla maskiner, använd `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Förbyggda binärer

Release-tillgångar publiceras för:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Ladda ner de senaste tillgångarna från:
<https://github.com/quant-speed/quantclaw/releases/latest>

## Dokumentation

Använd dessa när du är förbi onboarding-flödet och vill ha den djupare referensen.

- Börja med [dokumentationsindexet](docs/README.md) för navigering och "vad finns var."
- Läs [arkitekturöversikten](docs/architecture.md) för den fullständiga systemmodellen.
- Använd [konfigurationsreferensen](docs/reference/api/config-reference.md) när du behöver varje nyckel och exempel.
- Kör Gateway enligt boken med [operationsrunbook](docs/ops/operations-runbook.md).
- Följ [QuantClaw Onboard](#snabbstart) för en guidad installation.
- Felsök vanliga problem med [felsökningsguiden](docs/ops/troubleshooting.md).
- Granska [säkerhetsvägledning](docs/security/README.md) innan du exponerar något.

### Referensdokumentation

- Dokumentationshubb: [docs/README.md](docs/README.md)
- Enhetlig dokumentations-TOC: [docs/SUMMARY.md](docs/SUMMARY.md)
- Kommandoreferens: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Konfigurationsreferens: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Leverantörsreferens: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Kanalreferens: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Operationsrunbook: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Felsökning: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Samarbetsdokumentation

- Bidragsguide: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR-arbetsflödespolicy: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI-arbetsflödesguide: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Granskningsplaybook: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Säkerhetsutlämnandepolicy: [SECURITY.md](SECURITY.md)
- Dokumentationsmall: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Distribution + drift

- Nätverksdistributionsguide: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Proxy-agentplaybook: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Hårdvaruguider: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw byggdes för smooth crab 🦀, en snabb och effektiv AI-assistent. Byggd av Argenis De La Rosa och gemenskapen.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## Stöd QuantClaw

Om QuantClaw hjälper ditt arbete och du vill stödja pågående utveckling kan du donera här:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 Särskilt tack

Ett hjärtligt tack till de gemenskaper och institutioner som inspirerar och driver detta open source-arbete:

- **Harvard University** — för att främja intellektuell nyfikenhet och tänja gränserna för vad som är möjligt.
- **MIT** — för att försvara öppen kunskap, öppen källkod och tron att teknologi bör vara tillgänglig för alla.
- **Sundai Club** — för gemenskapen, energin och den outtröttliga driften att bygga saker som spelar roll.
- **Världen & bortom** 🌍✨ — till varje bidragsgivare, drömmare och byggare där ute som gör öppen källkod till en kraft för gott. Det här är för er.

Vi bygger öppet eftersom de bästa idéerna kommer från överallt. Om du läser detta är du en del av det. Välkommen. 🦀❤️

## Bidra

Ny till QuantClaw? Leta efter ärenden märkta [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — se vår [Bidragsguide](CONTRIBUTING.md#first-time-contributors) för hur du kommer igång. AI/vibe-kodade PR:er är välkomna! 🤖

Se [CONTRIBUTING.md](CONTRIBUTING.md) och [CLA.md](docs/contributing/cla.md). Implementera en trait, skicka in en PR:

- CI-arbetsflödesguide: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Ny `Provider` → `src/providers/`
- Ny `Channel` → `src/channels/`
- Ny `Observer` → `src/observability/`
- Nytt `Tool` → `src/tools/`
- Nytt `Memory` → `src/memory/`
- Ny `Tunnel` → `src/tunnel/`
- Ny `Peripheral` → `src/peripherals/`
- Ny `Skill` → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Officiellt repository & varning för imitation

**Detta är det enda officiella QuantClaw-repositoryt:**

> https://github.com/quant-speed/quantclaw

Alla andra repositorier, organisationer, domäner eller paket som hävdar att vara "QuantClaw" eller antyder anslutning till QuantClaw Labs är **obehöriga och inte affilierade med detta projekt**. Kända obehöriga forkar listas i [TRADEMARK.md](docs/maintainers/trademark.md).

Om du stöter på imitation eller varumärkesmissbruk, vänligen [öppna ett ärende](https://github.com/quant-speed/quantclaw/issues).

---

## Licens

QuantClaw är dubbellicensierat för maximal öppenhet och bidragsgivarskydd:

| Licens | Användningsfall |
|--------|-----------------|
| [MIT](LICENSE-MIT) | Öppen källkod, forskning, akademiskt, personligt bruk |
| [Apache 2.0](LICENSE-APACHE) | Patentskydd, institutionell, kommersiell distribution |

Du kan välja endera licens. **Bidragsgivare beviljar automatiskt rättigheter under båda** — se [CLA.md](docs/contributing/cla.md) för det fullständiga bidragsgivaravtalet.

### Varumärke

**QuantClaw**-namnet och logotypen är varumärken som tillhör QuantClaw Labs. Denna licens beviljar inte tillstånd att använda dem för att antyda stöd eller anslutning. Se [TRADEMARK.md](docs/maintainers/trademark.md) för tillåtna och förbjudna användningar.

### Bidragsgivarskydd

- Du **behåller upphovsrätten** till dina bidrag
- **Patentbeviljande** (Apache 2.0) skyddar dig från patentkrav från andra bidragsgivare
- Dina bidrag är **permanent tillskrivna** i commit-historik och [NOTICE](NOTICE)
- Inga varumärkesrättigheter överförs genom att bidra

---

**QuantClaw** — Noll overhead. Noll kompromiss. Distribuera var som helst. Byt ut vad som helst. 🦀

## Bidragsgivare

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw-bidragsgivare" />
</a>

Denna lista genereras från GitHub-bidragsgivargrafen och uppdateras automatiskt.

## Stjärnhistorik

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
