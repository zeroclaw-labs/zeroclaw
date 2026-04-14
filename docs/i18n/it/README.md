<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — Assistente Personale IA</h1>

<p align="center">
  <strong>Zero overhead. Zero compromessi. 100% Rust. 100% Agnostico.</strong><br>
  ⚡️ <strong>Funziona su hardware da $10 con <5MB di RAM: il 99% in meno di memoria rispetto a OpenClaw e il 98% più economico di un Mac mini!</strong>
</p>

<p align="center">
Costruito da studenti e membri delle comunità di Harvard, MIT e Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Lingue:</strong>
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

QuantClaw è un assistente personale IA che esegui sui tuoi dispositivi. Ti risponde sui canali che già usi (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work e altri). Ha una dashboard web per il controllo in tempo reale e può connettersi a periferiche hardware (ESP32, STM32, Arduino, Raspberry Pi). Il Gateway è solo il piano di controllo — il prodotto è l'assistente.

Se vuoi un assistente personale, per un singolo utente, che sia locale, veloce e sempre attivo, questo fa per te.

<p align="center">
  <a href="https://quantspeed.ai">Sito web</a> ·
  <a href="docs/README.md">Documentazione</a> ·
  <a href="docs/architecture.md">Architettura</a> ·
  <a href="#avvio-rapido">Per iniziare</a> ·
  <a href="#migrazione-da-openclaw">Migrazione da OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Risoluzione problemi</a> ·
</p>

> **Configurazione consigliata:** esegui `quantclaw onboard` nel tuo terminale. QuantClaw Onboard ti guida passo dopo passo nella configurazione del gateway, workspace, canali e provider. È il percorso di configurazione consigliato e funziona su macOS, Linux e Windows (tramite WSL2). Nuova installazione? Inizia qui: [Per iniziare](#avvio-rapido)

### Autenticazione tramite abbonamento (OAuth)

- **OpenAI Codex** (abbonamento ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (chiave API o token di autenticazione)

Nota sui modelli: sebbene siano supportati molti provider/modelli, per la migliore esperienza usa il modello di ultima generazione più potente a tua disposizione. Vedi [Onboarding](#avvio-rapido).

Configurazione modelli + CLI: [Riferimento provider](docs/reference/api/providers-reference.md)
Rotazione profili di autenticazione (OAuth vs chiavi API) + failover: [Failover modelli](docs/reference/api/providers-reference.md)

## Installazione (consigliata)

Requisito: toolchain stabile di Rust. Un singolo binario, nessuna dipendenza di runtime.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### Bootstrap con un clic

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` viene eseguito automaticamente dopo l'installazione per configurare il tuo workspace e provider.

## Avvio rapido (TL;DR)

Guida completa per principianti (autenticazione, accoppiamento, canali): [Per iniziare](docs/setup-guides/one-click-bootstrap.md)

```bash
# Installa + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Avvia il gateway (server webhook + dashboard web)
quantclaw gateway                # predefinito: 127.0.0.1:42617
quantclaw gateway --port 0       # porta casuale (sicurezza rafforzata)

# Parla con l'assistente
quantclaw agent -m "Hello, QuantClaw!"

# Modalità interattiva
quantclaw agent

# Avvia il runtime autonomo completo (gateway + canali + cron + hands)
quantclaw daemon

# Controlla lo stato
quantclaw status

# Esegui diagnostica
quantclaw doctor
```

Aggiornamento? Esegui `quantclaw doctor` dopo l'aggiornamento.

### Dal codice sorgente (sviluppo)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **Alternativa per lo sviluppo (senza installazione globale):** anteponi `cargo run --release --` ai comandi (esempio: `cargo run --release -- status`).

## Migrazione da OpenClaw

QuantClaw può importare il tuo workspace, memoria e configurazione da OpenClaw:

```bash
# Anteprima di ciò che verrà migrato (sicuro, sola lettura)
quantclaw migrate openclaw --dry-run

# Esegui la migrazione
quantclaw migrate openclaw
```

Questo migra le tue voci di memoria, i file del workspace e la configurazione da `~/.openclaw/` a `~/.quantclaw/`. La configurazione viene convertita da JSON a TOML automaticamente.

## Impostazioni di sicurezza predefinite (accesso DM)

QuantClaw si connette a superfici di messaggistica reali. Tratta i DM in arrivo come input non attendibile.

Guida completa alla sicurezza: [SECURITY.md](SECURITY.md)

Comportamento predefinito su tutti i canali:

- **Accoppiamento DM** (predefinito): i mittenti sconosciuti ricevono un breve codice di accoppiamento e il bot non elabora il loro messaggio.
- Approva con: `quantclaw pairing approve <channel> <code>` (il mittente viene quindi aggiunto a una allowlist locale).
- I DM pubblici in arrivo richiedono un'attivazione esplicita in `config.toml`.
- Esegui `quantclaw doctor` per individuare politiche DM rischiose o mal configurate.

**Livelli di autonomia:**

| Livello | Comportamento |
|---------|---------------|
| `ReadOnly` | L'agente può osservare ma non agire |
| `Supervised` (predefinito) | L'agente agisce con approvazione per operazioni a rischio medio/alto |
| `Full` | L'agente agisce autonomamente entro i limiti della policy |

**Livelli di sandboxing:** isolamento del workspace, blocco del traversal dei percorsi, allowlist dei comandi, percorsi proibiti (`/etc`, `/root`, `~/.ssh`), limitazione della velocità (max azioni/ora, tetti di costo/giorno).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Annunci

Usa questa bacheca per avvisi importanti (breaking change, avvisi di sicurezza, finestre di manutenzione e bloccanti del rilascio).

| Data (UTC) | Livello       | Avviso                                                                                                                                                                                                                                                                                                                                                 | Azione                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Critico_  | **Non siamo affiliati** con `openagen/quantclaw`, `quantclaw.org` o `quantclaw.net`. I domini `quantclaw.org` e `quantclaw.net` attualmente puntano al fork `openagen/quantclaw`, e quel dominio/repository stanno impersonando il nostro sito web/progetto ufficiale.                                                                                       | Non fidarti di informazioni, binari, raccolte fondi o annunci da quelle fonti. Usa solo [questo repository](https://github.com/quant-speed/quantclaw) e i nostri account social verificati.                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Importante_ | Anthropic ha aggiornato i termini di Autenticazione e Uso delle Credenziali il 2026-02-19. I token OAuth di Claude Code (Free, Pro, Max) sono destinati esclusivamente a Claude Code e Claude.ai; usare token OAuth di Claude Free/Pro/Max in qualsiasi altro prodotto, strumento o servizio (incluso Agent SDK) non è consentito e può violare i Termini di Servizio del Consumatore. | Per favore, evita temporaneamente le integrazioni OAuth di Claude Code per prevenire potenziali perdite. Clausola originale: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                                                    |

## Punti di forza

- **Runtime leggero per impostazione predefinita** — i flussi di lavoro comuni di CLI e stato funzionano in pochi megabyte di memoria nelle build release.
- **Distribuzione economica** — progettato per schede da $10 e piccole istanze cloud, nessuna dipendenza di runtime pesante.
- **Avvio a freddo rapido** — il runtime Rust a binario singolo mantiene l'avvio dei comandi e del daemon quasi istantaneo.
- **Architettura portabile** — un binario per ARM, x86 e RISC-V con provider/canali/strumenti intercambiabili.
- **Gateway local-first** — piano di controllo unico per sessioni, canali, strumenti, cron, SOP ed eventi.
- **Casella di posta multicanale** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket e altri.
- **Orchestrazione multi-agente (Hands)** — sciami di agenti autonomi che funzionano secondo programma e diventano più intelligenti nel tempo.
- **Procedure Operative Standard (SOP)** — automazione dei flussi di lavoro guidata da eventi con MQTT, webhook, cron e trigger dei periferici.
- **Dashboard web** — interfaccia web React 19 + Vite con chat in tempo reale, browser della memoria, editor di configurazione, gestore cron e ispettore degli strumenti.
- **Periferiche hardware** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO tramite il trait `Peripheral`.
- **Strumenti di prima classe** — shell, I/O file, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace e oltre 70 altri.
- **Hook del ciclo di vita** — intercetta e modifica chiamate LLM, esecuzioni di strumenti e messaggi in ogni fase.
- **Piattaforma skill** — skill incluse, della community e del workspace con audit di sicurezza.
- **Supporto tunnel** — Cloudflare, Tailscale, ngrok, OpenVPN e tunnel personalizzati per l'accesso remoto.

### Perché i team scelgono QuantClaw

- **Leggero per impostazione predefinita:** binario Rust piccolo, avvio rapido, basso consumo di memoria.
- **Sicuro per design:** accoppiamento, sandboxing rigoroso, allowlist esplicite, scoping del workspace.
- **Completamente intercambiabile:** i sistemi centrali sono trait (provider, canali, strumenti, memoria, tunnel).
- **Nessun vendor lock-in:** supporto provider compatibili con OpenAI + endpoint personalizzati collegabili.

## Riepilogo benchmark (QuantClaw vs OpenClaw, riproducibile)

Benchmark rapido su macchina locale (macOS arm64, feb 2026) normalizzato per hardware edge a 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Linguaggio**            | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Avvio (core 0.8GHz)**  | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Dimensione binario**   | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Costo**                | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Qualsiasi hardware $10** |

> Note: I risultati di QuantClaw sono misurati su build release usando `/usr/bin/time -l`. OpenClaw richiede il runtime Node.js (tipicamente ~390MB di overhead di memoria aggiuntivo), mentre NanoBot richiede il runtime Python. PicoClaw e QuantClaw sono binari statici. I valori di RAM sopra sono memoria a runtime; i requisiti di compilazione sono superiori.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### Misurazione locale riproducibile

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## Tutto ciò che abbiamo costruito finora

### Piattaforma centrale

- Piano di controllo Gateway HTTP/WS/SSE con sessioni, presenza, configurazione, cron, webhook, dashboard web e accoppiamento.
- Superficie CLI: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Loop di orchestrazione dell'agente con dispatch degli strumenti, costruzione dei prompt, classificazione dei messaggi e caricamento della memoria.
- Modello di sessione con applicazione delle policy di sicurezza, livelli di autonomia e approvazione condizionale.
- Wrapper provider resiliente con failover, retry e routing dei modelli su oltre 20 backend LLM.

### Canali

Canali: WhatsApp (nativo), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Abilitati tramite feature gate: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Dashboard web

Dashboard web React 19 + Vite 6 + Tailwind CSS 4 servita direttamente dal Gateway:

- **Dashboard** — panoramica del sistema, stato di salute, uptime, tracciamento dei costi
- **Chat dell'agente** — chat interattiva con l'agente
- **Memoria** — esplora e gestisci le voci di memoria
- **Configurazione** — visualizza e modifica la configurazione
- **Cron** — gestisci attività programmate
- **Strumenti** — esplora gli strumenti disponibili
- **Log** — visualizza i log di attività dell'agente
- **Costi** — utilizzo dei token e tracciamento dei costi
- **Doctor** — diagnostica della salute del sistema
- **Integrazioni** — stato e configurazione delle integrazioni
- **Accoppiamento** — gestione dell'accoppiamento dei dispositivi

### Obiettivi firmware

| Obiettivo | Piattaforma | Scopo |
|-----------|-------------|-------|
| ESP32 | Espressif ESP32 | Agente periferico wireless |
| ESP32-UI | ESP32 + Display | Agente con interfaccia visiva |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Periferico industriale |
| Arduino | Arduino | Ponte base sensori/attuatori |
| Uno Q Bridge | Arduino Uno | Ponte seriale verso l'agente |

### Strumenti + automazione

- **Core:** shell, lettura/scrittura/modifica file, operazioni git, ricerca glob, ricerca contenuti
- **Web:** controllo browser, web fetch, web search, screenshot, informazioni immagine, lettura PDF
- **Integrazioni:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + set di strumenti differiti
- **Programmazione:** cron add/remove/update/run, strumento di programmazione
- **Memoria:** recall, store, forget, knowledge, project intel
- **Avanzato:** delegate (agente-a-agente), swarm, cambio/routing modelli, operazioni di sicurezza, operazioni cloud
- **Hardware:** board info, memory map, memory read (abilitato tramite feature gate)

### Runtime + sicurezza

- **Livelli di autonomia:** ReadOnly, Supervised (predefinito), Full.
- **Sandboxing:** isolamento del workspace, blocco del traversal dei percorsi, allowlist dei comandi, percorsi proibiti, Landlock (Linux), Bubblewrap.
- **Limitazione della velocità:** max azioni per ora, max costo per giorno (configurabile).
- **Approvazione condizionale:** approvazione interattiva per operazioni a rischio medio/alto.
- **Arresto di emergenza:** capacità di spegnimento di emergenza.
- **129+ test di sicurezza** in CI automatizzato.

### Operazioni + packaging

- Dashboard web servita direttamente dal Gateway.
- Supporto tunnel: Cloudflare, Tailscale, ngrok, OpenVPN, comando personalizzato.
- Adattatore runtime Docker per esecuzione in container.
- CI/CD: beta (automatico al push) → stable (dispatch manuale) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Binari precompilati per Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Configurazione

`~/.quantclaw/config.toml` minimo:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Riferimento completo della configurazione: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Configurazione dei canali

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

### Configurazione dei tunnel

```toml
[tunnel]
kind = "cloudflare"  # o "tailscale", "ngrok", "openvpn", "custom", "none"
```

Dettagli: [Riferimento canali](docs/reference/api/channels-reference.md) · [Riferimento configurazione](docs/reference/api/config-reference.md)

### Supporto runtime (attuale)

- **`native`** (predefinito) — esecuzione diretta dei processi, percorso più veloce, ideale per ambienti fidati.
- **`docker`** — isolamento completo in container, policy di sicurezza forzate, richiede Docker.

Imposta `runtime.kind = "docker"` per sandboxing rigoroso o isolamento di rete.

## Autenticazione tramite abbonamento (OpenAI Codex / Claude Code / Gemini)

QuantClaw supporta profili di autenticazione nativi tramite abbonamento (multi-account, crittografati a riposo).

- File di archiviazione: `~/.quantclaw/auth-profiles.json`
- Chiave di crittografia: `~/.quantclaw/.secret_key`
- Formato id profilo: `<provider>:<profile_name>` (esempio: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (abbonamento ChatGPT)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Controlla / aggiorna / cambia profilo
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Esegui l'agente con autenticazione tramite abbonamento
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## Workspace dell'agente + skill

Root del workspace: `~/.quantclaw/workspace/` (configurabile tramite config).

File di prompt iniettati:
- `IDENTITY.md` — personalità e ruolo dell'agente
- `USER.md` — contesto e preferenze dell'utente
- `MEMORY.md` — fatti e lezioni a lungo termine
- `AGENTS.md` — convenzioni di sessione e regole di inizializzazione
- `SOUL.md` — identità centrale e principi operativi

Skill: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` o `SKILL.toml`.

```bash
# Elenca le skill installate
quantclaw skills list

# Installa da git
quantclaw skills install https://github.com/user/my-skill.git

# Audit di sicurezza prima dell'installazione
quantclaw skills audit https://github.com/user/my-skill.git

# Rimuovi una skill
quantclaw skills remove my-skill
```

## Comandi CLI

```bash
# Gestione del workspace
quantclaw onboard              # Procedura guidata di configurazione
quantclaw status               # Mostra stato del daemon/agente
quantclaw doctor               # Esegui diagnostica del sistema

# Gateway + daemon
quantclaw gateway              # Avvia server gateway (127.0.0.1:42617)
quantclaw daemon               # Avvia runtime autonomo completo

# Agente
quantclaw agent                # Modalità chat interattiva
quantclaw agent -m "message"   # Modalità messaggio singolo

# Gestione servizi
quantclaw service install      # Installa come servizio del SO (launchd/systemd)
quantclaw service start|stop|restart|status

# Canali
quantclaw channel list         # Elenca i canali configurati
quantclaw channel doctor       # Controlla la salute dei canali
quantclaw channel bind-telegram 123456789

# Cron + programmazione
quantclaw cron list            # Elenca i lavori programmati
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Memoria
quantclaw memory list          # Elenca le voci di memoria
quantclaw memory get <key>     # Recupera una memoria
quantclaw memory stats         # Statistiche della memoria

# Profili di autenticazione
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Periferiche hardware
quantclaw hardware discover    # Scansiona i dispositivi connessi
quantclaw peripheral list      # Elenca le periferiche connesse
quantclaw peripheral flash     # Flash del firmware sul dispositivo

# Migrazione
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Completamento shell
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

Riferimento completo dei comandi: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Prerequisiti

<details>
<summary><strong>Windows</strong></summary>

#### Richiesto

1. **Visual Studio Build Tools** (fornisce il linker MSVC e il Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Durante l'installazione (o tramite il Visual Studio Installer), seleziona il carico di lavoro **"Sviluppo desktop con C++"**.

2. **Toolchain di Rust:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Dopo l'installazione, apri un nuovo terminale ed esegui `rustup default stable` per assicurarti che la toolchain stabile sia attiva.

3. **Verifica** che entrambi funzionino:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opzionale

- **Docker Desktop** — necessario solo se usi il [runtime sandbox con Docker](#supporto-runtime-attuale) (`runtime.kind = "docker"`). Installa tramite `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Richiesto

1. **Strumenti di compilazione essenziali:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Installa Xcode Command Line Tools: `xcode-select --install`

2. **Toolchain di Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Vedi [rustup.rs](https://rustup.rs) per i dettagli.

3. **Verifica** che entrambi funzionino:
    ```bash
    rustc --version
    cargo --version
    ```

#### Installatore in una riga

Oppure salta i passaggi precedenti e installa tutto (dipendenze di sistema, Rust, QuantClaw) con un solo comando:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### Requisiti di risorse per la compilazione

Compilare dal codice sorgente richiede più risorse rispetto all'esecuzione del binario risultante:

| Risorsa        | Minimo  | Consigliato |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Disco libero**| 6 GB   | 10 GB+      |

Se il tuo host è al di sotto del minimo, usa i binari precompilati:

```bash
./install.sh --prefer-prebuilt
```

Per richiedere l'installazione solo da binari senza compilazione di fallback:

```bash
./install.sh --prebuilt-only
```

#### Opzionale

- **Docker** — necessario solo se usi il [runtime sandbox con Docker](#supporto-runtime-attuale) (`runtime.kind = "docker"`). Installa tramite il tuo gestore di pacchetti o [docker.com](https://docs.docker.com/engine/install/).

> **Nota:** Il `cargo build --release` predefinito usa `codegen-units=1` per ridurre la pressione massima di compilazione. Per build più veloci su macchine potenti, usa `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Binari precompilati

Gli asset di release sono pubblicati per:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Scarica gli ultimi asset da:
<https://github.com/quant-speed/quantclaw/releases/latest>

## Documentazione

Usa queste risorse quando hai superato il flusso di onboarding e vuoi il riferimento più approfondito.

- Inizia con l'[indice della documentazione](docs/README.md) per la navigazione e "cosa c'è dove."
- Leggi la [panoramica dell'architettura](docs/architecture.md) per il modello completo del sistema.
- Usa il [riferimento della configurazione](docs/reference/api/config-reference.md) quando hai bisogno di ogni chiave ed esempio.
- Esegui il Gateway secondo il libro con il [runbook operativo](docs/ops/operations-runbook.md).
- Segui [QuantClaw Onboard](#avvio-rapido) per una configurazione guidata.
- Risolvi errori comuni con la [guida alla risoluzione dei problemi](docs/ops/troubleshooting.md).
- Rivedi la [guida alla sicurezza](docs/security/README.md) prima di esporre qualsiasi cosa.

### Documentazione di riferimento

- Hub della documentazione: [docs/README.md](docs/README.md)
- TOC unificato dei docs: [docs/SUMMARY.md](docs/SUMMARY.md)
- Riferimento comandi: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Riferimento configurazione: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Riferimento provider: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Riferimento canali: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Runbook operativo: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Risoluzione problemi: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Documentazione di collaborazione

- Guida alla contribuzione: [CONTRIBUTING.md](CONTRIBUTING.md)
- Politica del flusso di lavoro PR: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- Guida al flusso di lavoro CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Manuale del revisore: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Politica di divulgazione della sicurezza: [SECURITY.md](SECURITY.md)
- Template della documentazione: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Distribuzione + operazioni

- Guida alla distribuzione in rete: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Manuale dell'agente proxy: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Guide hardware: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw è stato costruito per il granchio liscio 🦀, un assistente IA veloce ed efficiente. Costruito da Argenis De La Rosa e la comunità.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## Supporta QuantClaw

Se QuantClaw ti aiuta nel lavoro e vuoi supportare lo sviluppo continuo, puoi donare qui:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 Ringraziamenti speciali

Un sentito ringraziamento alle comunità e alle istituzioni che ispirano e alimentano questo lavoro open source:

- **Harvard University** — per alimentare la curiosità intellettuale e spingere i confini del possibile.
- **MIT** — per difendere la conoscenza aperta, l'open source e la convinzione che la tecnologia debba essere accessibile a tutti.
- **Sundai Club** — per la comunità, l'energia e la spinta instancabile a costruire cose che contano.
- **Il Mondo e Oltre** 🌍✨ — a ogni contributore, sognatore e costruttore che rende l'open source una forza per il bene. Questo è per te.

Stiamo costruendo apertamente perché le migliori idee vengono da ovunque. Se stai leggendo questo, ne fai parte. Benvenuto. 🦀❤️

## Contribuire

Nuovo su QuantClaw? Cerca le issue etichettate [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — consulta la nostra [Guida alla contribuzione](CONTRIBUTING.md#first-time-contributors) per sapere come iniziare. PR con IA/vibe-coded sono benvenuti! 🤖

Vedi [CONTRIBUTING.md](CONTRIBUTING.md) e [CLA.md](docs/contributing/cla.md). Implementa un trait, invia un PR:

- Guida al flusso di lavoro CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Nuovo `Provider` → `src/providers/`
- Nuovo `Channel` → `src/channels/`
- Nuovo `Observer` → `src/observability/`
- Nuovo `Tool` → `src/tools/`
- Nuovo `Memory` → `src/memory/`
- Nuovo `Tunnel` → `src/tunnel/`
- Nuovo `Peripheral` → `src/peripherals/`
- Nuovo `Skill` → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Repository ufficiale e avviso di impersonificazione

**Questo è l'unico repository ufficiale di QuantClaw:**

> https://github.com/quant-speed/quantclaw

Qualsiasi altro repository, organizzazione, dominio o pacchetto che affermi di essere "QuantClaw" o implichi un'affiliazione con QuantClaw Labs **non è autorizzato e non è affiliato a questo progetto**. I fork non autorizzati conosciuti saranno elencati in [TRADEMARK.md](docs/maintainers/trademark.md).

Se incontri impersonificazione o uso improprio del marchio, per favore [apri una issue](https://github.com/quant-speed/quantclaw/issues).

---

## Licenza

QuantClaw ha doppia licenza per massima apertura e protezione dei contributori:

| Licenza | Caso d'uso |
|---|---|
| [MIT](LICENSE-MIT) | Open source, ricerca, accademico, uso personale |
| [Apache 2.0](LICENSE-APACHE) | Protezione brevetti, istituzionale, distribuzione commerciale |

Puoi scegliere una delle due licenze. **I contributori concedono automaticamente diritti sotto entrambe** — vedi [CLA.md](docs/contributing/cla.md) per l'accordo completo dei contributori.

### Marchio

Il nome e il logo di **QuantClaw** sono marchi di QuantClaw Labs. Questa licenza non concede il permesso di usarli per implicare approvazione o affiliazione. Vedi [TRADEMARK.md](docs/maintainers/trademark.md) per gli usi consentiti e proibiti.

### Protezioni per i contributori

- **Mantieni il copyright** delle tue contribuzioni
- **Concessione di brevetti** (Apache 2.0) ti protegge da rivendicazioni di brevetti di altri contributori
- Le tue contribuzioni sono **permanentemente attribuite** nella cronologia dei commit e [NOTICE](NOTICE)
- Nessun diritto di marchio viene trasferito contribuendo

---

**QuantClaw** — Zero overhead. Zero compromessi. Distribuisci ovunque. Scambia qualsiasi cosa. 🦀

## Contributori

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

Questa lista è generata dal grafico dei contributori di GitHub e si aggiorna automaticamente.

## Cronologia delle stelle

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
