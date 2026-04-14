<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — Persoonlijke AI-Assistent</h1>

<p align="center">
  <strong>Nul overhead. Nul compromis. 100% Rust. 100% Agnostisch.</strong><br>
  ⚡️ <strong>Draait op $10 hardware met <5MB RAM: Dat is 99% minder geheugen dan OpenClaw en 98% goedkoper dan een Mac mini!</strong>
</p>

<p align="center">
Gebouwd door studenten en leden van de Harvard-, MIT- en Sundai.Club-gemeenschappen.
</p>

<p align="center">
  🌐 <strong>Talen:</strong>
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

QuantClaw is een persoonlijke AI-assistent die je op je eigen apparaten draait. Hij beantwoordt je op de kanalen die je al gebruikt (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work en meer). Het heeft een webdashboard voor realtime controle en kan verbinding maken met hardware-randapparatuur (ESP32, STM32, Arduino, Raspberry Pi). De Gateway is slechts het besturingsvlak — het product is de assistent.

Als je een persoonlijke, single-user assistent wilt die lokaal, snel en altijd beschikbaar aanvoelt — dit is het.

<p align="center">
  <a href="https://quantspeed.ai">Website</a> ·
  <a href="docs/README.md">Documentatie</a> ·
  <a href="docs/architecture.md">Architectuur</a> ·
  <a href="#snelle-start">Aan de slag</a> ·
  <a href="#migreren-van-openclaw">Migreren van OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Probleemoplossing</a> ·
</p>

> **Aanbevolen setup:** voer `quantclaw onboard` uit in je terminal. QuantClaw Onboard begeleidt je stap voor stap door het instellen van de gateway, workspace, kanalen en provider. Het is het aanbevolen installatiepad en werkt op macOS, Linux en Windows (via WSL2). Nieuwe installatie? Begin hier: [Aan de slag](#snelle-start)

### Abonnementsauthenticatie (OAuth)

- **OpenAI Codex** (ChatGPT-abonnement)
- **Gemini** (Google OAuth)
- **Anthropic** (API-sleutel of autorisatietoken)

Modelopmerking: hoewel veel providers/modellen worden ondersteund, gebruik voor de beste ervaring het sterkste beschikbare model van de nieuwste generatie. Zie [Onboarding](#snelle-start).

Modelconfiguratie + CLI: [Providers-referentie](docs/reference/api/providers-reference.md)
Autorisatieprofiel-rotatie (OAuth vs API-sleutels) + failover: [Model-failover](docs/reference/api/providers-reference.md)

## Installatie (aanbevolen)

Runtime: stabiele Rust-toolchain. Enkel binair bestand, geen runtime-afhankelijkheden.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### Installatie met één klik

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` wordt automatisch uitgevoerd na installatie om je workspace en provider te configureren.

## Snelle start (TL;DR)

Volledige beginnersgids (authenticatie, koppeling, kanalen): [Aan de slag](docs/setup-guides/one-click-bootstrap.md)

```bash
# Installatie + onboarding
./install.sh --api-key "sk-..." --provider openrouter

# Start de gateway (webhook-server + webdashboard)
quantclaw gateway                # standaard: 127.0.0.1:42617
quantclaw gateway --port 0       # willekeurige poort (beveiligingsversterkt)

# Praat met de assistent
quantclaw agent -m "Hello, QuantClaw!"

# Interactieve modus
quantclaw agent

# Start volledige autonome runtime (gateway + kanalen + cron + hands)
quantclaw daemon

# Controleer status
quantclaw status

# Voer diagnostiek uit
quantclaw doctor
```

Bijwerken? Voer `quantclaw doctor` uit na het updaten.

### Vanuit broncode (ontwikkeling)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **Dev-fallback (geen globale installatie):** voeg `cargo run --release --` voor commando's toe (voorbeeld: `cargo run --release -- status`).

## Migreren van OpenClaw

QuantClaw kan je OpenClaw-workspace, geheugen en configuratie importeren:

```bash
# Voorbeeld van wat gemigreerd wordt (veilig, alleen-lezen)
quantclaw migrate openclaw --dry-run

# Voer de migratie uit
quantclaw migrate openclaw
```

Dit migreert je geheugenregistraties, workspace-bestanden en configuratie van `~/.openclaw/` naar `~/.quantclaw/`. Configuratie wordt automatisch geconverteerd van JSON naar TOML.

## Standaard beveiligingsinstellingen (DM-toegang)

QuantClaw verbindt met echte berichtenplatforms. Behandel inkomende DM's als onbetrouwbare invoer.

Volledige beveiligingsgids: [SECURITY.md](SECURITY.md)

Standaardgedrag op alle kanalen:

- **DM-koppeling** (standaard): onbekende afzenders ontvangen een korte koppelingscode en de bot verwerkt hun bericht niet.
- Goedkeuren met: `quantclaw pairing approve <channel> <code>` (vervolgens wordt de afzender toegevoegd aan een lokale allowlist).
- Publieke inkomende DM's vereisen een expliciete opt-in in `config.toml`.
- Voer `quantclaw doctor` uit om riskante of verkeerd geconfigureerde DM-beleidsregels te detecteren.

**Autonomieniveaus:**

| Niveau | Gedrag |
|--------|--------|
| `ReadOnly` | Agent kan observeren maar niet handelen |
| `Supervised` (standaard) | Agent handelt met goedkeuring voor medium/hoog risico-operaties |
| `Full` | Agent handelt autonoom binnen beleidsgrenzen |

**Sandboxing-lagen:** workspace-isolatie, padtraversatieblokkering, commando-allowlisting, verboden paden (`/etc`, `/root`, `~/.ssh`), snelheidsbeperking (max acties/uur, kosten/dag-limieten).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Aankondigingen

Gebruik dit bord voor belangrijke mededelingen (breaking changes, beveiligingsadviezen, onderhoudsvensters en release-blokkers).

| Datum (UTC) | Niveau       | Mededeling                                                                                                                                                                                                                                                                                                                                                 | Actie                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Kritiek_  | We zijn **niet gelieerd** aan `openagen/quantclaw`, `quantclaw.org` of `quantclaw.net`. De domeinen `quantclaw.org` en `quantclaw.net` verwijzen momenteel naar de `openagen/quantclaw`-fork, en dat domein/repository doet zich voor als onze officiële website/project.                                                                                       | Vertrouw geen informatie, binaire bestanden, fondswerving of aankondigingen van die bronnen. Gebruik alleen [dit repository](https://github.com/quant-speed/quantclaw) en onze geverifieerde sociale accounts.                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Belangrijk_ | Anthropic heeft de voorwaarden voor authenticatie en gebruik van inloggegevens bijgewerkt op 2026-02-19. Claude Code OAuth-tokens (Free, Pro, Max) zijn uitsluitend bedoeld voor Claude Code en Claude.ai; het gebruik van OAuth-tokens van Claude Free/Pro/Max in elk ander product, tool of service (inclusief Agent SDK) is niet toegestaan en kan de Consumentenvoorwaarden schenden. | Vermijd tijdelijk Claude Code OAuth-integraties om potentieel verlies te voorkomen. Originele clausule: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                    |

## Hoogtepunten

- **Lichte runtime standaard** — veelvoorkomende CLI- en statusworkflows draaien in een geheugenomvang van enkele megabytes op release-builds.
- **Kostenefficiënte implementatie** — ontworpen voor $10-borden en kleine cloud-instances, geen zware runtime-afhankelijkheden.
- **Snelle koude starts** — single-binary Rust-runtime houdt het opstarten van commando's en daemon vrijwel instant.
- **Draagbare architectuur** — één binair bestand voor ARM, x86 en RISC-V met verwisselbare providers/kanalen/tools.
- **Lokale gateway** — enkel besturingsvlak voor sessies, kanalen, tools, cron, SOP's en events.
- **Multi-channel inbox** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket en meer.
- **Multi-agent-orkestratie (Hands)** — autonome agentenzwermen die op schema draaien en na verloop van tijd slimmer worden.
- **Standaard Operationele Procedures (SOP's)** — event-gedreven workflowautomatisering met MQTT-, webhook-, cron- en periferie-triggers.
- **Webdashboard** — React 19 + Vite web-UI met realtime chat, geheugenbrowser, configuratie-editor, cron-manager en tool-inspector.
- **Hardware-randapparatuur** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO via de `Peripheral`-trait.
- **Eersteklas tools** — shell, bestands-I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace en 70+ meer.
- **Lifecycle-hooks** — onderschep en wijzig LLM-aanroepen, tool-uitvoeringen en berichten in elke fase.
- **Skills-platform** — ingebouwde, community- en workspace-skills met beveiligingsaudit.
- **Tunnelondersteuning** — Cloudflare, Tailscale, ngrok, OpenVPN en aangepaste tunnels voor externe toegang.

### Waarom teams kiezen voor QuantClaw

- **Licht standaard:** klein Rust-binair bestand, snelle opstart, laag geheugengebruik.
- **Veilig by design:** koppeling, strikte sandboxing, expliciete allowlists, workspace-scoping.
- **Volledig verwisselbaar:** kernsystemen zijn traits (providers, kanalen, tools, geheugen, tunnels).
- **Geen vendor lock-in:** OpenAI-compatibele provider-ondersteuning + inplugbare aangepaste endpoints.

## Benchmark-overzicht (QuantClaw vs OpenClaw, reproduceerbaar)

Snelle lokale benchmark (macOS arm64, feb 2026) genormaliseerd voor 0.8GHz edge-hardware.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Taal**                  | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Opstart (0.8GHz core)** | > 500s       | > 30s          | < 1s            | **< 10ms**           |
| **Binaire grootte**       | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Kosten**                | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Elke hardware $10** |

> Opmerkingen: QuantClaw-resultaten zijn gemeten op release-builds met `/usr/bin/time -l`. OpenClaw vereist Node.js-runtime (typisch ~390MB extra geheugenoverhead), terwijl NanoBot Python-runtime vereist. PicoClaw en QuantClaw zijn statische binaries. De RAM-cijfers hierboven zijn runtime-geheugen; compilatievereisten zijn hoger.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### Reproduceerbare lokale meting

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## Alles wat we tot nu toe hebben gebouwd

### Kernplatform

- Gateway HTTP/WS/SSE besturingsvlak met sessies, aanwezigheid, configuratie, cron, webhooks, webdashboard en koppeling.
- CLI-oppervlak: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Agent-orkestratielus met tool-dispatch, promptconstructie, berichtclassificatie en geheugen laden.
- Sessiemodel met beveiligingsbeleid-handhaving, autonomieniveaus en goedkeuringspoorten.
- Veerkrachtige provider-wrapper met failover, retry en modelrouting over 20+ LLM-backends.

### Kanalen

Kanalen: WhatsApp (natief), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Feature-gated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Webdashboard

React 19 + Vite 6 + Tailwind CSS 4 webdashboard geserveerd direct vanuit de Gateway:

- **Dashboard** — systeemoverzicht, gezondheidsstatus, uptime, kostentracking
- **Agent Chat** — interactieve chat met de agent
- **Geheugen** — bladeren en beheren van geheugenregistraties
- **Configuratie** — bekijken en bewerken van configuratie
- **Cron** — beheer van geplande taken
- **Tools** — bladeren door beschikbare tools
- **Logs** — bekijken van agent-activiteitslogs
- **Kosten** — tokengebruik en kostentracking
- **Doctor** — systeemgezondheidsdiagnostiek
- **Integraties** — integratiestatus en setup
- **Koppeling** — apparaatkoppelingsbeheer

### Firmware-doelen

| Doel | Platform | Doel |
|------|----------|------|
| ESP32 | Espressif ESP32 | Draadloze perifere agent |
| ESP32-UI | ESP32 + Display | Agent met visuele interface |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Industriële periferie |
| Arduino | Arduino | Basis sensor/actuator-brug |
| Uno Q Bridge | Arduino Uno | Seriële brug naar agent |

### Tools + automatisering

- **Kern:** shell, bestand lezen/schrijven/bewerken, git-operaties, glob-zoekopdracht, inhoudszoekopdracht
- **Web:** browserbediening, web fetch, webzoekopdracht, screenshot, afbeeldingsinfo, PDF lezen
- **Integraties:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool-wrapper + uitgestelde toolsets
- **Planning:** cron add/remove/update/run, planningstool
- **Geheugen:** recall, store, forget, knowledge, project intel
- **Geavanceerd:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **Hardware:** board info, memory map, memory read (feature-gated)

### Runtime + veiligheid

- **Autonomieniveaus:** ReadOnly, Supervised (standaard), Full.
- **Sandboxing:** workspace-isolatie, padtraversatieblokkering, commando-allowlists, verboden paden, Landlock (Linux), Bubblewrap.
- **Snelheidsbeperking:** max acties per uur, max kosten per dag (configureerbaar).
- **Goedkeuringspoort:** interactieve goedkeuring voor medium/hoog risico-operaties.
- **E-stop:** noodstopfunctionaliteit.
- **129+ beveiligingstests** in geautomatiseerd CI.

### Ops + verpakking

- Webdashboard geserveerd direct vanuit de Gateway.
- Tunnelondersteuning: Cloudflare, Tailscale, ngrok, OpenVPN, aangepast commando.
- Docker runtime-adapter voor gecontaineriseerde uitvoering.
- CI/CD: beta (auto bij push) → stable (handmatige dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Voorgebouwde binaries voor Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Configuratie

Minimale `~/.quantclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Volledige configuratiereferentie: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Kanaalconfiguratie

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

### Tunnelconfiguratie

```toml
[tunnel]
kind = "cloudflare"  # of "tailscale", "ngrok", "openvpn", "custom", "none"
```

Details: [Kanaalreferentie](docs/reference/api/channels-reference.md) · [Configuratiereferentie](docs/reference/api/config-reference.md)

### Runtime-ondersteuning (huidig)

- **`native`** (standaard) — directe procesuitvoering, snelste pad, ideaal voor vertrouwde omgevingen.
- **`docker`** — volledige containerisolatie, afgedwongen beveiligingsbeleid, vereist Docker.

Stel `runtime.kind = "docker"` in voor strikte sandboxing of netwerkisolatie.

## Abonnementsauthenticatie (OpenAI Codex / Claude Code / Gemini)

QuantClaw ondersteunt native abonnementsautorisatieprofielen (meerdere accounts, versleuteld in rust).

- Opslagbestand: `~/.quantclaw/auth-profiles.json`
- Versleutelingssleutel: `~/.quantclaw/.secret_key`
- Profiel-ID-formaat: `<provider>:<profile_name>` (voorbeeld: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT-abonnement)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Controleer / ververs / wissel profiel
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Agent draaien met abonnementsauth
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## Agent-workspace + skills

Workspace-root: `~/.quantclaw/workspace/` (configureerbaar via config).

Geïnjecteerde promptbestanden:
- `IDENTITY.md` — persoonlijkheid en rol van de agent
- `USER.md` — gebruikerscontext en voorkeuren
- `MEMORY.md` — langetermijnfeiten en lessen
- `AGENTS.md` — sessieconventies en initialisatieregels
- `SOUL.md` — kernidentiteit en operationele principes

Skills: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` of `SKILL.toml`.

```bash
# Lijst geïnstalleerde skills
quantclaw skills list

# Installeer vanuit git
quantclaw skills install https://github.com/user/my-skill.git

# Beveiligingsaudit voor installatie
quantclaw skills audit https://github.com/user/my-skill.git

# Verwijder een skill
quantclaw skills remove my-skill
```

## CLI-commando's

```bash
# Workspace-beheer
quantclaw onboard              # Begeleide installatiewizard
quantclaw status               # Toon daemon/agent-status
quantclaw doctor               # Voer systeemdiagnostiek uit

# Gateway + daemon
quantclaw gateway              # Start gateway-server (127.0.0.1:42617)
quantclaw daemon               # Start volledige autonome runtime

# Agent
quantclaw agent                # Interactieve chatmodus
quantclaw agent -m "message"   # Enkele berichtmodus

# Servicebeheer
quantclaw service install      # Installeer als OS-service (launchd/systemd)
quantclaw service start|stop|restart|status

# Kanalen
quantclaw channel list         # Lijst geconfigureerde kanalen
quantclaw channel doctor       # Controleer kanaalgezondheid
quantclaw channel bind-telegram 123456789

# Cron + planning
quantclaw cron list            # Lijst geplande taken
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Geheugen
quantclaw memory list          # Lijst geheugenregistraties
quantclaw memory get <key>     # Haal een geheugenitem op
quantclaw memory stats         # Geheugenstatistieken

# Autorisatieprofielen
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Hardware-randapparatuur
quantclaw hardware discover    # Scan verbonden apparaten
quantclaw peripheral list      # Lijst verbonden randapparatuur
quantclaw peripheral flash     # Flash firmware naar apparaat

# Migratie
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Shell-aanvullingen
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

Volledige commandoreferentie: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Vereisten

<details>
<summary><strong>Windows</strong></summary>

#### Vereist

1. **Visual Studio Build Tools** (biedt de MSVC-linker en Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Selecteer tijdens de installatie (of via de Visual Studio Installer) de **"Desktop development with C++"** workload.

2. **Rust-toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Open na installatie een nieuwe terminal en voer `rustup default stable` uit om te verzekeren dat de stabiele toolchain actief is.

3. **Controleer** of beide werken:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Optioneel

- **Docker Desktop** — alleen vereist bij gebruik van de [Docker-sandboxed runtime](#runtime-ondersteuning-huidig) (`runtime.kind = "docker"`). Installeer via `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Vereist

1. **Bouwtools:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Installeer Xcode Command Line Tools: `xcode-select --install`

2. **Rust-toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Zie [rustup.rs](https://rustup.rs) voor details.

3. **Controleer** of beide werken:
    ```bash
    rustc --version
    cargo --version
    ```

#### Eenregelige installer

Of sla bovenstaande stappen over en installeer alles (systeemafhankelijkheden, Rust, QuantClaw) in één commando:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### Compilatieresource-vereisten

Bouwen vanuit broncode heeft meer resources nodig dan het draaien van het resulterende binaire bestand:

| Resource       | Minimum | Aanbevolen  |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Vrije schijf** | 6 GB  | 10 GB+      |

Als je host onder het minimum zit, gebruik dan voorgebouwde binaries:

```bash
./install.sh --prefer-prebuilt
```

Om alleen binaire installatie te forceren zonder broncode-fallback:

```bash
./install.sh --prebuilt-only
```

#### Optioneel

- **Docker** — alleen vereist bij gebruik van de [Docker-sandboxed runtime](#runtime-ondersteuning-huidig) (`runtime.kind = "docker"`). Installeer via je pakketbeheerder of [docker.com](https://docs.docker.com/engine/install/).

> **Opmerking:** De standaard `cargo build --release` gebruikt `codegen-units=1` om piekcompiledruk te verlagen. Voor snellere builds op krachtige machines, gebruik `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Voorgebouwde binaries

Release-assets worden gepubliceerd voor:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Download de nieuwste assets van:
<https://github.com/quant-speed/quantclaw/releases/latest>

## Documentatie

Gebruik deze wanneer je voorbij de onboarding bent en diepere referentie wilt.

- Begin met de [documentatie-index](docs/README.md) voor navigatie en "wat staat waar."
- Lees het [architectuuroverzicht](docs/architecture.md) voor het volledige systeemmodel.
- Gebruik de [configuratiereferentie](docs/reference/api/config-reference.md) wanneer je elke sleutel en elk voorbeeld nodig hebt.
- Draai de Gateway volgens het [operationele draaiboek](docs/ops/operations-runbook.md).
- Volg [QuantClaw Onboard](#snelle-start) voor een begeleide setup.
- Debug veelvoorkomende fouten met de [probleemoplossingsgids](docs/ops/troubleshooting.md).
- Bekijk de [beveiligingsrichtlijnen](docs/security/README.md) voordat je iets blootstelt.

### Referentiedocumentatie

- Documentatiehub: [docs/README.md](docs/README.md)
- Uniforme inhoudsopgave: [docs/SUMMARY.md](docs/SUMMARY.md)
- Commandoreferentie: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Configuratiereferentie: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Providerreferentie: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Kanaalreferentie: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Operationeel draaiboek: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Probleemoplossing: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Samenwerkingsdocumentatie

- Bijdragegids: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR-workflowbeleid: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI-workflowgids: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Reviewer-draaiboek: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Beveiligingsonthullingsbeleid: [SECURITY.md](SECURITY.md)
- Documentatiesjabloon: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Implementatie + operaties

- Netwerkimplementatiegids: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Proxy-agent-draaiboek: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Hardwaregidsen: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw is gebouwd voor de smooth crab 🦀, een snelle en efficiënte AI-assistent. Gebouwd door Argenis De La Rosa en de gemeenschap.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## Steun QuantClaw

Als QuantClaw je werk helpt en je de voortdurende ontwikkeling wilt steunen, kun je hier doneren:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 Speciale dank

Een hartelijk dankjewel aan de gemeenschappen en instellingen die dit open-source werk inspireren en voeden:

- **Harvard University** — voor het bevorderen van intellectuele nieuwsgierigheid en het verleggen van de grenzen van het mogelijke.
- **MIT** — voor het verdedigen van open kennis, open source en het geloof dat technologie voor iedereen toegankelijk moet zijn.
- **Sundai Club** — voor de gemeenschap, de energie en de onvermoeibare drang om dingen te bouwen die ertoe doen.
- **De wereld en verder** 🌍✨ — aan elke bijdrager, dromer en bouwer die open source een kracht ten goede maakt. Dit is voor jou.

We bouwen in het open omdat de beste ideeën overal vandaan komen. Als je dit leest, ben je er onderdeel van. Welkom. 🦀❤️

## Bijdragen

Nieuw bij QuantClaw? Zoek naar issues gelabeld [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — zie onze [Bijdragegids](CONTRIBUTING.md#first-time-contributors) om te beginnen. AI/vibe-coded PR's welkom! 🤖

Zie [CONTRIBUTING.md](CONTRIBUTING.md) en [CLA.md](docs/contributing/cla.md). Implementeer een trait, dien een PR in:

- CI-workflowgids: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Nieuwe `Provider` → `src/providers/`
- Nieuw `Channel` → `src/channels/`
- Nieuwe `Observer` → `src/observability/`
- Nieuwe `Tool` → `src/tools/`
- Nieuw `Memory` → `src/memory/`
- Nieuwe `Tunnel` → `src/tunnel/`
- Nieuw `Peripheral` → `src/peripherals/`
- Nieuwe `Skill` → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Officieel repository & waarschuwing tegen imitatie

**Dit is het enige officiële QuantClaw-repository:**

> https://github.com/quant-speed/quantclaw

Elk ander repository, organisatie, domein of pakket dat beweert "QuantClaw" te zijn of een relatie met QuantClaw Labs impliceert, is **ongeautoriseerd en niet gelieerd aan dit project**. Bekende ongeautoriseerde forks worden vermeld in [TRADEMARK.md](docs/maintainers/trademark.md).

Als je imitatie of merkmisbruik tegenkomt, [open dan een issue](https://github.com/quant-speed/quantclaw/issues).

---

## Licentie

QuantClaw heeft een dubbele licentie voor maximale openheid en bescherming van bijdragers:

| Licentie | Gebruiksscenario |
|----------|-------------------|
| [MIT](LICENSE-MIT) | Open-source, onderzoek, academisch, persoonlijk gebruik |
| [Apache 2.0](LICENSE-APACHE) | Octrooi-bescherming, institutioneel, commerciële implementatie |

Je kunt een van beide licenties kiezen. **Bijdragers verlenen automatisch rechten onder beide** — zie [CLA.md](docs/contributing/cla.md) voor de volledige bijdrager-overeenkomst.

### Handelsmerk

De **QuantClaw**-naam en het logo zijn handelsmerken van QuantClaw Labs. Deze licentie verleent geen toestemming om ze te gebruiken om goedkeuring of affiliatie te impliceren. Zie [TRADEMARK.md](docs/maintainers/trademark.md) voor toegestaan en verboden gebruik.

### Bijdragerbescherming

- Je **behoudt het auteursrecht** op je bijdragen
- **Octrooiverlening** (Apache 2.0) beschermt je tegen octrooiclaims van andere bijdragers
- Je bijdragen worden **permanent toegeschreven** in de commitgeschiedenis en [NOTICE](NOTICE)
- Er worden geen handelsmerkrechten overgedragen door bij te dragen

---

**QuantClaw** — Nul overhead. Nul compromis. Implementeer overal. Wissel alles. 🦀

## Bijdragers

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

Deze lijst wordt gegenereerd vanuit de GitHub-bijdragersgrafiek en wordt automatisch bijgewerkt.

## Sterrengeschiedenis

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
