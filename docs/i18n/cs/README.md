<p align="center">
  <img src="https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/docs/assets/daemonclaw-banner.png" alt="DaemonClaw" width="600" />
</p>

<h1 align="center">🦀 DaemonClaw — Osobní AI Asistent</h1>

<p align="center">
  <strong>Nulová režie. Nulový kompromis. 100% Rust. 100% Agnostický.</strong><br>
  ⚡️ <strong>Běží na hardwaru za $10 s <5MB RAM: To je o 99 % méně paměti než OpenClaw a o 98 % levnější než Mac mini!</strong>
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
Vytvořeno studenty a členy komunit Harvard, MIT a Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Jazyky:</strong>
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

DaemonClaw je osobní AI asistent, který spouštíte na vlastních zařízeních. Odpovídá vám na kanálech, které již používáte (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work a další). Má webový panel pro řízení v reálném čase a může se připojit k hardwarovým periferiím (ESP32, STM32, Arduino, Raspberry Pi). Gateway je pouze řídicí rovina — produktem je asistent.

Pokud hledáte osobního jednouživatelského asistenta, který je lokální, rychlý a vždy dostupný — toto je ono.

<p align="center">
  <a href="https://daemonclawlabs.ai">Webové stránky</a> ·
  <a href="docs/README.md">Dokumentace</a> ·
  <a href="docs/architecture.md">Architektura</a> ·
  <a href="#rychlý-start">Začínáme</a> ·
  <a href="#migrace-z-openclaw">Migrace z OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Řešení problémů</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **Doporučené nastavení:** spusťte `daemonclaw onboard` ve vašem terminálu. DaemonClaw Onboard vás krok za krokem provede nastavením gateway, workspace, kanálů a poskytovatele. Je to doporučená cesta nastavení a funguje na macOS, Linux a Windows (přes WSL2). Nová instalace? Začněte zde: [Začínáme](#rychlý-start)

### Autentizace předplatného (OAuth)

- **OpenAI Codex** (předplatné ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (API klíč nebo autorizační token)

Poznámka k modelům: ačkoli je podporováno mnoho poskytovatelů/modelů, pro nejlepší zážitek použijte nejsilnější dostupný model nejnovější generace. Viz [Onboarding](#rychlý-start).

Konfigurace modelů + CLI: [Reference poskytovatelů](docs/reference/api/providers-reference.md)
Rotace autorizačních profilů (OAuth vs API klíče) + failover: [Failover modelů](docs/reference/api/providers-reference.md)

## Instalace (doporučená)

Běhové prostředí: stabilní toolchain Rust. Jeden binární soubor, žádné runtime závislosti.

### Homebrew (macOS/Linuxbrew)

```bash
brew install daemonclaw
```

### Instalace jedním kliknutím

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw
./install.sh
```

`daemonclaw onboard` se automaticky spustí po instalaci pro konfiguraci vašeho workspace a poskytovatele.

## Rychlý start (TL;DR)

Kompletní průvodce pro začátečníky (autentizace, párování, kanály): [Začínáme](docs/setup-guides/one-click-bootstrap.md)

```bash
# Instalace + onboarding
./install.sh --api-key "sk-..." --provider openrouter

# Spuštění gateway (webhook server + webový panel)
daemonclaw gateway                # výchozí: 127.0.0.1:42617
daemonclaw gateway --port 0       # náhodný port (posílené zabezpečení)

# Komunikace s asistentem
daemonclaw agent -m "Hello, DaemonClaw!"

# Interaktivní režim
daemonclaw agent

# Spuštění plného autonomního běhového prostředí (gateway + kanály + cron + hands)
daemonclaw daemon

# Kontrola stavu
daemonclaw status

# Spuštění diagnostiky
daemonclaw doctor
```

Aktualizujete? Spusťte `daemonclaw doctor` po aktualizaci.

### Ze zdrojového kódu (vývoj)

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw

cargo build --release --locked
cargo install --path . --force --locked

daemonclaw onboard
```

> **Vývojářský fallback (bez globální instalace):** předřaďte příkazy `cargo run --release --` (příklad: `cargo run --release -- status`).

## Migrace z OpenClaw

DaemonClaw může importovat váš workspace, paměť a konfiguraci OpenClaw:

```bash
# Náhled toho, co bude migrováno (bezpečné, pouze čtení)
daemonclaw migrate openclaw --dry-run

# Spuštění migrace
daemonclaw migrate openclaw
```

Migruje záznamy paměti, soubory workspace a konfiguraci z `~/.openclaw/` do `~/.daemonclaw/`. Konfigurace je automaticky převedena z JSON do TOML.

## Výchozí nastavení zabezpečení (přístup DM)

DaemonClaw se připojuje k reálným komunikačním platformám. Zacházejte s příchozími DM jako s nedůvěryhodným vstupem.

Kompletní průvodce zabezpečením: [SECURITY.md](SECURITY.md)

Výchozí chování na všech kanálech:

- **Párování DM** (výchozí): neznámí odesílatelé obdrží krátký párovací kód a bot nezpracovává jejich zprávu.
- Schvalte pomocí: `daemonclaw pairing approve <channel> <code>` (poté je odesílatel přidán na lokální allowlist).
- Veřejné příchozí DM vyžadují explicitní opt-in v `config.toml`.
- Spusťte `daemonclaw doctor` pro odhalení rizikových nebo špatně nakonfigurovaných DM politik.

**Úrovně autonomie:**

| Úroveň | Chování |
|--------|---------|
| `ReadOnly` | Agent může pozorovat, ale nemůže jednat |
| `Supervised` (výchozí) | Agent jedná se schválením pro operace se středním/vysokým rizikem |
| `Full` | Agent jedná autonomně v rámci hranic politiky |

**Vrstvy sandboxingu:** izolace workspace, blokování procházení cest, allowlisty příkazů, zakázané cesty (`/etc`, `/root`, `~/.ssh`), omezení rychlosti (max akcí/hodinu, denní limity nákladů).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Oznámení

Používejte tuto nástěnku pro důležitá oznámení (zlomové změny, bezpečnostní upozornění, okna údržby a blokátory vydání).

| Datum (UTC) | Úroveň       | Oznámení                                                                                                                                                                                                                                                                                                                                                 | Akce                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Kritické_  | **Nejsme spojeni** s `openagen/daemonclaw`, `daemonclaw.org` ani `daemonclaw.net`. Domény `daemonclaw.org` a `daemonclaw.net` aktuálně směřují na fork `openagen/daemonclaw` a tato doména/repozitář se vydávají za naši oficiální stránku/projekt.                                                                                       | Nedůvěřujte informacím, binárním souborům, sbírkám ani oznámením z těchto zdrojů. Používejte pouze [toto repozitárium](https://github.com/DeliveryBoyTech/daemonclaw) a naše ověřené sociální účty.                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Důležité_ | Anthropic aktualizoval podmínky autentizace a použití přihlašovacích údajů 2026-02-19. OAuth tokeny Claude Code (Free, Pro, Max) jsou určeny výhradně pro Claude Code a Claude.ai; používání OAuth tokenů z Claude Free/Pro/Max v jakémkoli jiném produktu, nástroji nebo službě (včetně Agent SDK) není povoleno a může porušovat Podmínky služby. | Prosím dočasně se vyhněte integracím Claude Code OAuth, abyste předešli potenciálním ztrátám. Původní klauzule: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                    |

## Hlavní rysy

- **Lehké běhové prostředí ve výchozím stavu** — běžné CLI a statusové workflow běží v obálce paměti několika megabajtů na release buildech.
- **Nákladově efektivní nasazení** — navrženo pro desky za $10 a malé cloudové instance, žádné těžké runtime závislosti.
- **Rychlé studené starty** — jednobinární Rust runtime udržuje start příkazů a démona téměř okamžitý.
- **Přenosná architektura** — jeden binární soubor pro ARM, x86 a RISC-V s vyměnitelnými poskytovateli/kanály/nástroji.
- **Lokální gateway** — jednotná řídicí rovina pro relace, kanály, nástroje, cron, SOP a události.
- **Vícekanálová schránka** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket a další.
- **Orchestrace více agentů (Hands)** — autonomní roje agentů, které běží podle plánu a časem se stávají chytřejšími.
- **Standardní operační postupy (SOP)** — automatizace workflow řízená událostmi s triggery MQTT, webhook, cron a periferiemi.
- **Webový panel** — rozhraní React 19 + Vite s chatem v reálném čase, prohlížečem paměti, editorem konfigurace, správcem cron a inspektorem nástrojů.
- **Hardwarové periferie** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO přes trait `Peripheral`.
- **Prvotřídní nástroje** — shell, souborové I/O, prohlížeč, git, web fetch/search, MCP, Jira, Notion, Google Workspace a 70+ dalších.
- **Lifecycle hooky** — zachytávejte a upravujte volání LLM, spouštění nástrojů a zprávy v každé fázi.
- **Platforma dovedností** — vestavěné, komunitní a workspace dovednosti s bezpečnostním auditem.
- **Podpora tunelů** — Cloudflare, Tailscale, ngrok, OpenVPN a vlastní tunely pro vzdálený přístup.

### Proč týmy volí DaemonClaw

- **Lehký ve výchozím stavu:** malý Rust binární soubor, rychlý start, nízká paměťová stopa.
- **Bezpečný od návrhu:** párování, přísný sandboxing, explicitní allowlisty, izolace workspace.
- **Plně vyměnitelný:** základní systémy jsou traity (poskytovatelé, kanály, nástroje, paměť, tunely).
- **Žádný vendor lock-in:** podpora poskytovatelů kompatibilních s OpenAI + připojitelné vlastní endpointy.

## Srovnání výkonu (DaemonClaw vs OpenClaw, reprodukovatelné)

Rychlý benchmark na lokálním stroji (macOS arm64, únor 2026) normalizovaný pro edge hardware 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | DaemonClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Jazyk**                 | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Start (jádro 0.8GHz)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Velikost binárky**      | ~28MB (dist)  | N/A (Skripty)  | ~8MB            | **~8.8 MB**          |
| **Náklady**               | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Jakýkoli hardware $10** |

> Poznámky: Výsledky DaemonClaw jsou měřeny na release buildech pomocí `/usr/bin/time -l`. OpenClaw vyžaduje běhové prostředí Node.js (typicky ~390MB dodatečné paměťové režie), zatímco NanoBot vyžaduje běhové prostředí Python. PicoClaw a DaemonClaw jsou statické binárky. Výše uvedené hodnoty RAM jsou runtime paměť; požadavky kompilace jsou vyšší.

<p align="center">
  <img src="docs/assets/daemonclaw-comparison.jpeg" alt="DaemonClaw vs OpenClaw Comparison" width="800" />
</p>

### Reprodukovatelné lokální měření

```bash
cargo build --release
ls -lh target/release/daemonclaw

/usr/bin/time -l target/release/daemonclaw --help
/usr/bin/time -l target/release/daemonclaw status
```

## Vše, co jsme dosud vytvořili

### Základní platforma

- Gateway HTTP/WS/SSE řídicí rovina s relacemi, přítomností, konfigurací, cron, webhooky, webovým panelem a párováním.
- CLI rozhraní: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Orchestrační smyčka agenta s dispatchem nástrojů, konstrukcí promptů, klasifikací zpráv a načítáním paměti.
- Model relací s vynucováním bezpečnostní politiky, úrovněmi autonomie a schvalovacím gatováním.
- Odolný wrapper poskytovatele s failoverem, opakováním a routingem modelů napříč 20+ LLM backendy.

### Kanály

Kanály: WhatsApp (nativní), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Za feature gate: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Webový panel

Webový panel React 19 + Vite 6 + Tailwind CSS 4 servírovaný přímo z Gateway:

- **Dashboard** — přehled systému, stav zdraví, uptime, sledování nákladů
- **Chat s agentem** — interaktivní chat s agentem
- **Paměť** — prohlížení a správa záznamů paměti
- **Konfigurace** — zobrazení a úprava konfigurace
- **Cron** — správa naplánovaných úloh
- **Nástroje** — prohlížení dostupných nástrojů
- **Logy** — zobrazení logů aktivity agenta
- **Náklady** — využití tokenů a sledování nákladů
- **Doctor** — diagnostika zdraví systému
- **Integrace** — stav a nastavení integrací
- **Párování** — správa párování zařízení

### Cíle firmwaru

| Cíl | Platforma | Účel |
|-----|-----------|------|
| ESP32 | Espressif ESP32 | Bezdrátový periferní agent |
| ESP32-UI | ESP32 + Displej | Agent s vizuálním rozhraním |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Průmyslová periferie |
| Arduino | Arduino | Základní můstek senzorů/aktuátorů |
| Uno Q Bridge | Arduino Uno | Sériový můstek k agentovi |

### Nástroje + automatizace

- **Základní:** shell, čtení/zápis/editace souborů, operace git, glob vyhledávání, vyhledávání obsahu
- **Web:** ovládání prohlížeče, web fetch, webové vyhledávání, snímek obrazovky, info o obrázku, čtení PDF
- **Integrace:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** wrapper nástrojů Model Context Protocol + odložené sady nástrojů
- **Plánování:** cron add/remove/update/run, nástroj plánování
- **Paměť:** recall, store, forget, knowledge, project intel
- **Pokročilé:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **Hardware:** board info, memory map, memory read (za feature gate)

### Běhové prostředí + bezpečnost

- **Úrovně autonomie:** ReadOnly, Supervised (výchozí), Full.
- **Sandboxing:** izolace workspace, blokování procházení cest, allowlisty příkazů, zakázané cesty, Landlock (Linux), Bubblewrap.
- **Omezení rychlosti:** max akcí za hodinu, max nákladů za den (konfigurovatelné).
- **Schvalovací gatování:** interaktivní schvalování operací se středním/vysokým rizikem.
- **E-stop:** schopnost nouzového vypnutí.
- **129+ bezpečnostních testů** v automatizovaném CI.

### Provoz + balíčkování

- Webový panel servírovaný přímo z Gateway.
- Podpora tunelů: Cloudflare, Tailscale, ngrok, OpenVPN, vlastní příkaz.
- Docker runtime adaptér pro kontejnerizované spouštění.
- CI/CD: beta (auto na push) → stable (ruční dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Předpřipravené binárky pro Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Konfigurace

Minimální `~/.daemonclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Kompletní reference konfigurace: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Konfigurace kanálů

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

### Konfigurace tunelu

```toml
[tunnel]
kind = "cloudflare"  # nebo "tailscale", "ngrok", "openvpn", "custom", "none"
```

Podrobnosti: [Reference kanálů](docs/reference/api/channels-reference.md) · [Reference konfigurace](docs/reference/api/config-reference.md)

### Podpora runtime (aktuální)

- **`native`** (výchozí) — přímé spouštění procesů, nejrychlejší cesta, ideální pro důvěryhodná prostředí.
- **`docker`** — plná kontejnerová izolace, vynucené bezpečnostní politiky, vyžaduje Docker.

Nastavte `runtime.kind = "docker"` pro přísný sandboxing nebo síťovou izolaci.

## Autentizace předplatného (OpenAI Codex / Claude Code / Gemini)

DaemonClaw podporuje nativní autorizační profily předplatného (více účtů, šifrování v klidu).

- Soubor úložiště: `~/.daemonclaw/auth-profiles.json`
- Šifrovací klíč: `~/.daemonclaw/.secret_key`
- Formát ID profilu: `<provider>:<profile_name>` (příklad: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (předplatné ChatGPT)
daemonclaw auth login --provider openai-codex --device-code

# Gemini OAuth
daemonclaw auth login --provider gemini --profile default

# Anthropic setup-token
daemonclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Kontrola / obnovení / přepnutí profilu
daemonclaw auth status
daemonclaw auth refresh --provider openai-codex --profile default
daemonclaw auth use --provider openai-codex --profile work

# Spuštění agenta s autentizací předplatného
daemonclaw agent --provider openai-codex -m "hello"
daemonclaw agent --provider anthropic -m "hello"
```

## Workspace agenta + dovednosti

Kořenový adresář workspace: `~/.daemonclaw/workspace/` (konfigurovatelné přes config).

Injektované soubory promptů:
- `IDENTITY.md` — osobnost a role agenta
- `USER.md` — kontext a preference uživatele
- `MEMORY.md` — dlouhodobá fakta a poučení
- `AGENTS.md` — konvence relací a inicializační pravidla
- `SOUL.md` — základní identita a provozní principy

Dovednosti: `~/.daemonclaw/workspace/skills/<skill>/SKILL.md` nebo `SKILL.toml`.

```bash
# Seznam nainstalovaných dovedností
daemonclaw skills list

# Instalace z git
daemonclaw skills install https://github.com/user/my-skill.git

# Bezpečnostní audit před instalací
daemonclaw skills audit https://github.com/user/my-skill.git

# Odebrání dovednosti
daemonclaw skills remove my-skill
```

## CLI příkazy

```bash
# Správa workspace
daemonclaw onboard              # Průvodce nastavením
daemonclaw status               # Zobrazení stavu démona/agenta
daemonclaw doctor               # Spuštění diagnostiky systému

# Gateway + démon
daemonclaw gateway              # Spuštění gateway serveru (127.0.0.1:42617)
daemonclaw daemon               # Spuštění plného autonomního runtime

# Agent
daemonclaw agent                # Interaktivní režim chatu
daemonclaw agent -m "message"   # Režim jedné zprávy

# Správa služeb
daemonclaw service install      # Instalace jako služba OS (launchd/systemd)
daemonclaw service start|stop|restart|status

# Kanály
daemonclaw channel list         # Seznam konfigurovaných kanálů
daemonclaw channel doctor       # Kontrola zdraví kanálů
daemonclaw channel bind-telegram 123456789

# Cron + plánování
daemonclaw cron list            # Seznam naplánovaných úloh
daemonclaw cron add "*/5 * * * *" --prompt "Check system health"
daemonclaw cron remove <id>

# Paměť
daemonclaw memory list          # Seznam záznamů paměti
daemonclaw memory get <key>     # Získání záznamu
daemonclaw memory stats         # Statistiky paměti

# Autorizační profily
daemonclaw auth login --provider <name>
daemonclaw auth status
daemonclaw auth use --provider <name> --profile <profile>

# Hardwarové periferie
daemonclaw hardware discover    # Skenování připojených zařízení
daemonclaw peripheral list      # Seznam připojených periferií
daemonclaw peripheral flash     # Flash firmwaru na zařízení

# Migrace
daemonclaw migrate openclaw --dry-run
daemonclaw migrate openclaw

# Doplňování shellu
source <(daemonclaw completions bash)
daemonclaw completions zsh > ~/.zfunc/_daemonclaw
```

Kompletní reference příkazů: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Předpoklady

<details>
<summary><strong>Windows</strong></summary>

#### Požadované

1. **Visual Studio Build Tools** (poskytuje MSVC linker a Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Během instalace (nebo přes Visual Studio Installer) vyberte workload **"Desktop development with C++"**.

2. **Toolchain Rust:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Po instalaci otevřete nový terminál a spusťte `rustup default stable`, abyste zajistili aktivní stabilní toolchain.

3. **Ověřte**, že obojí funguje:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Volitelné

- **Docker Desktop** — požadován pouze při použití [Docker sandboxovaného runtime](#podpora-runtime-aktuální) (`runtime.kind = "docker"`). Instalace přes `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Požadované

1. **Nástroje pro sestavení:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Instalace Xcode Command Line Tools: `xcode-select --install`

2. **Toolchain Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Viz [rustup.rs](https://rustup.rs) pro podrobnosti.

3. **Ověřte**, že obojí funguje:
    ```bash
    rustc --version
    cargo --version
    ```

#### Jednořádkový instalátor

Nebo přeskočte výše uvedené kroky a nainstalujte vše (systémové závislosti, Rust, DaemonClaw) jedním příkazem:

```bash
curl -LsSf https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/install.sh | bash
```

#### Požadavky na zdroje kompilace

Sestavení ze zdrojového kódu vyžaduje více zdrojů než spuštění výsledné binárky:

| Zdroj          | Minimum | Doporučeno  |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Volné místo** | 6 GB   | 10 GB+      |

Pokud je váš host pod minimem, použijte předpřipravené binárky:

```bash
./install.sh --prefer-prebuilt
```

Pro vynucení instalace pouze z binárky bez fallbacku na zdrojový kód:

```bash
./install.sh --prebuilt-only
```

#### Volitelné

- **Docker** — požadován pouze při použití [Docker sandboxovaného runtime](#podpora-runtime-aktuální) (`runtime.kind = "docker"`). Instalace přes správce balíčků nebo [docker.com](https://docs.docker.com/engine/install/).

> **Poznámka:** Výchozí `cargo build --release` používá `codegen-units=1` pro snížení špičkového zatížení kompilace. Pro rychlejší buildy na výkonných strojích použijte `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Předpřipravené binárky

Vydané assety jsou publikovány pro:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Stáhněte nejnovější assety z:
<https://github.com/DeliveryBoyTech/daemonclaw/releases/latest>

## Dokumentace

Používejte tyto, když jste prošli onboardingem a chcete hlubší referenci.

- Začněte s [indexem dokumentace](docs/README.md) pro navigaci a „co je kde."
- Přečtěte si [přehled architektury](docs/architecture.md) pro úplný model systému.
- Použijte [referenci konfigurace](docs/reference/api/config-reference.md), když potřebujete každý klíč a příklad.
- Provozujte Gateway podle [provozní příručky](docs/ops/operations-runbook.md).
- Následujte [DaemonClaw Onboard](#rychlý-start) pro průvodce nastavením.
- Odlaďte běžné chyby s [průvodcem řešením problémů](docs/ops/troubleshooting.md).
- Projděte [bezpečnostní pokyny](docs/security/README.md) před vystavením čehokoli.

### Referenční dokumentace

- Centrum dokumentace: [docs/README.md](docs/README.md)
- Ujednocený obsah: [docs/SUMMARY.md](docs/SUMMARY.md)
- Reference příkazů: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Reference konfigurace: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Reference poskytovatelů: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Reference kanálů: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Provozní příručka: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Řešení problémů: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Dokumentace spolupráce

- Průvodce přispíváním: [CONTRIBUTING.md](CONTRIBUTING.md)
- Politika PR workflow: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- Průvodce CI workflow: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Příručka recenzenta: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Politika bezpečnostního zveřejnění: [SECURITY.md](SECURITY.md)
- Šablona dokumentace: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Nasazení + provoz

- Průvodce síťovým nasazením: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Příručka proxy agenta: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Hardwarové průvodce: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

DaemonClaw byl vytvořen pro smooth crab 🦀, rychlého a efektivního AI asistenta. Vytvořil Argenis De La Rosa a komunita.

- [daemonclawlabs.ai](https://daemonclawlabs.ai)
- [@daemonclawlabs](https://x.com/daemonclawlabs)

## Podpořte DaemonClaw

### 🙏 Speciální poděkování

Srdečné poděkování komunitám a institucím, které inspirují a pohánějí tuto open-source práci:

- **Harvard University** — za podporu intelektuální zvědavosti a posouvání hranic toho, co je možné.
- **MIT** — za prosazování otevřených znalostí, open source a víry, že technologie by měla být dostupná všem.
- **Sundai Club** — za komunitu, energii a neúnavný drive budovat věci, na kterých záleží.
- **Svět a dále** 🌍✨ — každému přispěvateli, snílkovi a tvůrci, kteří dělají z open source sílu dobra. Toto je pro vás.

Stavíme otevřeně, protože nejlepší nápady přicházejí odevšad. Pokud toto čtete, jste toho součástí. Vítejte. 🦀❤️

## Přispívání

Jste v DaemonClaw noví? Hledejte issues označené [`good first issue`](https://github.com/DeliveryBoyTech/daemonclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — podívejte se na náš [Průvodce přispíváním](CONTRIBUTING.md#first-time-contributors), jak začít. AI/vibe-coded PR vítány! 🤖

Viz [CONTRIBUTING.md](CONTRIBUTING.md) a [CLA.md](docs/contributing/cla.md). Implementujte trait, odešlete PR:

- Průvodce CI workflow: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Nový `Provider` → `src/providers/`
- Nový `Channel` → `src/channels/`
- Nový `Observer` → `src/observability/`
- Nový `Tool` → `src/tools/`
- Nový `Memory` → `src/memory/`
- Nový `Tunnel` → `src/tunnel/`
- Nový `Peripheral` → `src/peripherals/`
- Nový `Skill` → `~/.daemonclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Oficiální repozitář a varování před podvržením identity

**Toto je jediný oficiální repozitář DaemonClaw:**

> https://github.com/DeliveryBoyTech/daemonclaw

Jakýkoli jiný repozitář, organizace, doména nebo balíček tvrdící, že je „DaemonClaw" nebo naznačující spojení se DaemonClaw Labs je **neautorizovaný a není spojen s tímto projektem**. Známé neautorizované forky budou uvedeny v [TRADEMARK.md](docs/maintainers/trademark.md).

Pokud narazíte na podvržení identity nebo zneužití ochranné známky, prosím [otevřete issue](https://github.com/DeliveryBoyTech/daemonclaw/issues).

---

## Licence

DaemonClaw je dvojitě licencován pro maximální otevřenost a ochranu přispěvatelů:

| Licence | Případ použití |
|---------|---------------|
| [MIT](LICENSE-MIT) | Open-source, výzkum, akademie, osobní použití |
| [Apache 2.0](LICENSE-APACHE) | Patentová ochrana, institucionální, komerční nasazení |

Můžete si vybrat kteroukoli licenci. **Přispěvatelé automaticky udělují práva pod oběma** — viz [CLA.md](docs/contributing/cla.md) pro úplnou dohodu přispěvatele.

### Ochranná známka

Název **DaemonClaw** a logo jsou ochranné známky DaemonClaw Labs. Tato licence neuděluje povolení k jejich použití pro naznačení podpory nebo spojení. Viz [TRADEMARK.md](docs/maintainers/trademark.md) pro povolená a zakázaná použití.

### Ochrana přispěvatelů

- **Zachováváte si autorská práva** ke svým příspěvkům
- **Udělení patentu** (Apache 2.0) vás chrání před patentovými nároky jiných přispěvatelů
- Vaše příspěvky jsou **trvale připsány** v historii commitů a [NOTICE](NOTICE)
- Přispíváním se nepřevádějí žádná práva k ochranné známce

---

**DaemonClaw** — Nulová režie. Nulový kompromis. Nasaďte kdekoli. Vyměňte cokoli. 🦀

## Přispěvatelé

<a href="https://github.com/DeliveryBoyTech/daemonclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=DeliveryBoyTech/daemonclaw" alt="DaemonClaw contributors" />
</a>

Tento seznam je generován z grafu přispěvatelů GitHub a aktualizuje se automaticky.

## Historie hvězd

<p align="center">
  <a href="https://www.star-history.com/#DeliveryBoyTech/daemonclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
