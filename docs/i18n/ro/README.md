<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — Asistent AI Personal</h1>

<p align="center">
  <strong>Zero overhead. Zero compromisuri. 100% Rust. 100% Agnostic.</strong><br>
  ⚡️ <strong>Rulează pe hardware de $10 cu <5MB RAM: Cu 99% mai puțină memorie decât OpenClaw și cu 98% mai ieftin decât un Mac mini!</strong>
</p>

<p align="center">
Construit de studenți și membri ai comunităților Harvard, MIT și Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Limbi:</strong>
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

QuantClaw este un asistent AI personal pe care îl rulezi pe propriile dispozitive. Îți răspunde pe canalele pe care le folosești deja (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work și altele). Are un panou web pentru control în timp real și se poate conecta la periferice hardware (ESP32, STM32, Arduino, Raspberry Pi). Gateway-ul este doar planul de control — produsul este asistentul.

Dacă vrei un asistent personal, pentru un singur utilizator, care se simte local, rapid și mereu activ, acesta este.

<p align="center">
  <a href="https://quantspeed.ai">Site web</a> ·
  <a href="docs/README.md">Documentație</a> ·
  <a href="docs/architecture.md">Arhitectură</a> ·
  <a href="#pornire-rapidă">Începe</a> ·
  <a href="#migrarea-de-la-openclaw">Migrare de la OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Depanare</a> ·
</p>

> **Configurare recomandată:** rulează `quantclaw onboard` în terminalul tău. QuantClaw Onboard te ghidează pas cu pas prin configurarea gateway-ului, workspace-ului, canalelor și provider-ului. Este calea de configurare recomandată și funcționează pe macOS, Linux și Windows (prin WSL2). Instalare nouă? Începe aici: [Începe](#pornire-rapidă)

### Autentificare prin abonament (OAuth)

- **OpenAI Codex** (abonament ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (cheie API sau token de autentificare)

Notă despre modele: deși sunt suportate multe provider-e/modele, pentru cea mai bună experiență folosește cel mai puternic model de ultimă generație disponibil. Vezi [Onboarding](#pornire-rapidă).

Configurare modele + CLI: [Referință Providers](docs/reference/api/providers-reference.md)
Rotație profil de autentificare (OAuth vs chei API) + failover: [Failover model](docs/reference/api/providers-reference.md)

## Instalare (recomandat)

Runtime: Rust stable toolchain. Binar unic, fără dependențe de runtime.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### Bootstrap cu un clic

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` rulează automat după instalare pentru a configura workspace-ul și provider-ul.

## Pornire rapidă (TL;DR)

Ghid complet pentru începători (autentificare, asociere, canale): [Începe](docs/setup-guides/one-click-bootstrap.md)

```bash
# Instalare + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Pornește gateway-ul (server webhook + panou web)
quantclaw gateway                # implicit: 127.0.0.1:42617
quantclaw gateway --port 0       # port aleatoriu (securitate îmbunătățită)

# Vorbește cu asistentul
quantclaw agent -m "Hello, QuantClaw!"

# Mod interactiv
quantclaw agent

# Pornește runtime-ul autonom complet (gateway + canale + cron + hands)
quantclaw daemon

# Verifică starea
quantclaw status

# Rulează diagnostice
quantclaw doctor
```

Actualizezi? Rulează `quantclaw doctor` după actualizare.

### Din sursă (dezvoltare)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **Alternativă dev (fără instalare globală):** prefixează comenzile cu `cargo run --release --` (exemplu: `cargo run --release -- status`).

## Migrarea de la OpenClaw

QuantClaw poate importa workspace-ul, memoria și configurația OpenClaw:

```bash
# Previzualizează ce va fi migrat (sigur, doar citire)
quantclaw migrate openclaw --dry-run

# Rulează migrarea
quantclaw migrate openclaw
```

Aceasta migrează intrările de memorie, fișierele workspace și configurația din `~/.openclaw/` în `~/.quantclaw/`. Configurația este convertită automat din JSON în TOML.

## Setări implicite de securitate (acces DM)

QuantClaw se conectează la suprafețe de mesagerie reale. Tratează DM-urile primite ca intrare neîncredere.

Ghid complet de securitate: [SECURITY.md](SECURITY.md)

Comportament implicit pe toate canalele:

- **Asociere DM** (implicit): expeditorii necunoscuți primesc un cod scurt de asociere și bot-ul nu procesează mesajul lor.
- Aprobă cu: `quantclaw pairing approve <channel> <code>` (apoi expeditorul este adăugat pe o listă de permisiuni locală).
- DM-urile publice primite necesită un opt-in explicit în `config.toml`.
- Rulează `quantclaw doctor` pentru a identifica politici DM riscante sau configurate greșit.

**Niveluri de autonomie:**

| Nivel | Comportament |
|-------|----------|
| `ReadOnly` | Agentul poate observa dar nu poate acționa |
| `Supervised` (implicit) | Agentul acționează cu aprobare pentru operațiuni de risc mediu/ridicat |
| `Full` | Agentul acționează autonom în limitele politicii |

**Straturi de sandboxing:** izolarea workspace-ului, blocarea traversării căilor, liste de permisiuni pentru comenzi, căi interzise (`/etc`, `/root`, `~/.ssh`), limitare de rată (acțiuni maxime/oră, limite de cost/zi).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Anunțuri

Folosește acest panou pentru notificări importante (schimbări care rup compatibilitatea, avize de securitate, ferestre de mentenanță și blocaje de lansare).

| Data (UTC) | Nivel       | Notificare                                                                                                                                                                                                                                                                                                                                                 | Acțiune                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Critic_  | Nu suntem **afiliați** cu `openagen/quantclaw`, `quantclaw.org` sau `quantclaw.net`. Domeniile `quantclaw.org` și `quantclaw.net` indică în prezent fork-ul `openagen/quantclaw`, iar acel domeniu/depozit se dă drept site-ul/proiectul nostru oficial.                                                                                       | Nu aveți încredere în informații, binare, strângeri de fonduri sau anunțuri din acele surse. Folosiți doar [acest depozit](https://github.com/quant-speed/quantclaw) și conturile noastre sociale verificate.                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Important_ | Anthropic a actualizat termenii de Autentificare și Utilizare a Credențialelor pe 2026-02-19. Token-urile OAuth Claude Code (Free, Pro, Max) sunt destinate exclusiv Claude Code și Claude.ai; utilizarea token-urilor OAuth din Claude Free/Pro/Max în orice alt produs, instrument sau serviciu (inclusiv Agent SDK) nu este permisă și poate încălca Termenii Serviciului pentru Consumatori. | Vă rugăm să evitați temporar integrările OAuth Claude Code pentru a preveni pierderi potențiale. Clauza originală: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                                                    |

## Puncte forte

- **Runtime ușor implicit** — fluxurile comune CLI și de stare rulează într-un plic de memorie de câțiva megabytes pe build-urile de lansare.
- **Implementare eficientă din punct de vedere al costurilor** — proiectat pentru plăci de $10 și instanțe cloud mici, fără dependențe runtime grele.
- **Porniri la rece rapide** — runtime-ul Rust cu binar unic menține pornirea comenzilor și daemon-ului aproape instantanee.
- **Arhitectură portabilă** — un singur binar pe ARM, x86 și RISC-V cu provider-e/canale/instrumente interschimbabile.
- **Gateway local-first** — plan de control unic pentru sesiuni, canale, instrumente, cron, SOP-uri și evenimente.
- **Inbox multi-canal** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket și altele.
- **Orchestrare multi-agent (Hands)** — roiuri de agenți autonomi care rulează programat și devin mai inteligenți în timp.
- **Proceduri Operaționale Standard (SOP-uri)** — automatizare de fluxuri de lucru bazată pe evenimente cu MQTT, webhook, cron și declanșatoare periferice.
- **Panou Web** — UI web React 19 + Vite cu chat în timp real, browser de memorie, editor de configurare, manager cron și inspector de instrumente.
- **Periferice hardware** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO prin trait-ul `Peripheral`.
- **Instrumente de primă clasă** — shell, file I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace și 70+ altele.
- **Hook-uri de ciclu de viață** — interceptează și modifică apelurile LLM, execuțiile de instrumente și mesajele la fiecare etapă.
- **Platformă de skill-uri** — skill-uri incluse, comunitare și de workspace cu audit de securitate.
- **Suport tunnel** — Cloudflare, Tailscale, ngrok, OpenVPN și tuneluri personalizate pentru acces la distanță.

### De ce echipele aleg QuantClaw

- **Ușor implicit:** binar Rust mic, pornire rapidă, amprentă de memorie redusă.
- **Sigur prin design:** asociere, sandboxing strict, liste de permisiuni explicite, limitarea workspace-ului.
- **Complet interschimbabil:** sistemele de bază sunt trait-uri (provider-e, canale, instrumente, memorie, tuneluri).
- **Fără lock-in:** suport provider compatibil OpenAI + endpoint-uri personalizate conectabile.

## Instantaneu Benchmark (QuantClaw vs OpenClaw, Reproductibil)

Benchmark rapid pe mașină locală (macOS arm64, feb 2026) normalizat pentru hardware edge 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Limbaj**                | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Pornire (nucleu 0.8GHz)** | > 500s     | > 30s          | < 1s            | **< 10ms**           |
| **Dimensiune binar**     | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Cost**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Orice hardware $10** |

> Note: Rezultatele QuantClaw sunt măsurate pe build-uri de lansare folosind `/usr/bin/time -l`. OpenClaw necesită runtime Node.js (de obicei ~390MB overhead suplimentar de memorie), în timp ce NanoBot necesită runtime Python. PicoClaw și QuantClaw sunt binare statice. Cifrele RAM de mai sus sunt memorie runtime; cerințele de compilare în timpul build-ului sunt mai mari.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### Măsurare locală reproductibilă

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## Tot ce am construit până acum

### Platformă de bază

- Plan de control HTTP/WS/SSE Gateway cu sesiuni, prezență, configurare, cron, webhook-uri, panou web și asociere.
- Suprafață CLI: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Buclă de orchestrare agent cu dispatch de instrumente, construcție de prompt, clasificare de mesaje și încărcare de memorie.
- Model de sesiune cu aplicarea politicii de securitate, niveluri de autonomie și aprobare condiționată.
- Wrapper provider rezilient cu failover, reîncercare și rutare de modele pe 20+ backend-uri LLM.

### Canale

Canale: WhatsApp (nativ), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Feature-gated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Panou web

Panou web React 19 + Vite 6 + Tailwind CSS 4 servit direct din Gateway:

- **Dashboard** — prezentare generală a sistemului, stare de sănătate, uptime, urmărire costuri
- **Agent Chat** — chat interactiv cu agentul
- **Memory** — navighează și gestionează intrările de memorie
- **Config** — vizualizează și editează configurația
- **Cron** — gestionează sarcinile programate
- **Tools** — navighează instrumentele disponibile
- **Logs** — vizualizează jurnalele de activitate ale agentului
- **Cost** — utilizarea token-urilor și urmărirea costurilor
- **Doctor** — diagnostice de sănătate a sistemului
- **Integrations** — starea integrărilor și configurare
- **Pairing** — gestionarea asocierii dispozitivelor

### Ținte firmware

| Țintă | Platformă | Scop |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | Agent periferic wireless |
| ESP32-UI | ESP32 + Display | Agent cu interfață vizuală |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Periferic industrial |
| Arduino | Arduino | Punte senzor/actuator de bază |
| Uno Q Bridge | Arduino Uno | Punte serială către agent |

### Instrumente + automatizare

- **De bază:** shell, file read/write/edit, operații git, glob search, content search
- **Web:** browser control, web fetch, web search, screenshot, image info, PDF read
- **Integrări:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + deferred tool sets
- **Programare:** cron add/remove/update/run, schedule tool
- **Memorie:** recall, store, forget, knowledge, project intel
- **Avansat:** delegate (agent-la-agent), swarm, model switch/routing, security ops, cloud ops
- **Hardware:** board info, memory map, memory read (feature-gated)

### Runtime + siguranță

- **Niveluri de autonomie:** ReadOnly, Supervised (implicit), Full.
- **Sandboxing:** izolarea workspace-ului, blocarea traversării căilor, liste de permisiuni pentru comenzi, căi interzise, Landlock (Linux), Bubblewrap.
- **Limitare de rată:** acțiuni maxime pe oră, cost maxim pe zi (configurabil).
- **Aprobare condiționată:** aprobare interactivă pentru operațiuni de risc mediu/ridicat.
- **E-stop:** capacitate de oprire de urgență.
- **129+ teste de securitate** în CI automatizat.

### Ops + împachetare

- Panou web servit direct din Gateway.
- Suport tunnel: Cloudflare, Tailscale, ngrok, OpenVPN, comandă personalizată.
- Adaptor runtime Docker pentru execuție containerizată.
- CI/CD: beta (automat la push) → stable (dispatch manual) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Binare pre-construite pentru Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Configurare

Minimal `~/.quantclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Referință completă de configurare: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Configurare canale

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

### Configurare tunnel

```toml
[tunnel]
kind = "cloudflare"  # sau "tailscale", "ngrok", "openvpn", "custom", "none"
```

Detalii: [Referință canale](docs/reference/api/channels-reference.md) · [Referință configurare](docs/reference/api/config-reference.md)

### Suport runtime (curent)

- **`native`** (implicit) — execuție directă a procesului, cea mai rapidă cale, ideală pentru medii de încredere.
- **`docker`** — izolare completă în container, politici de securitate aplicate, necesită Docker.

Setează `runtime.kind = "docker"` pentru sandboxing strict sau izolare de rețea.

## Autentificare prin abonament (OpenAI Codex / Claude Code / Gemini)

QuantClaw suportă profiluri de autentificare native abonament (multi-cont, criptate în repaus).

- Fișier de stocare: `~/.quantclaw/auth-profiles.json`
- Cheie de criptare: `~/.quantclaw/.secret_key`
- Format id profil: `<provider>:<profile_name>` (exemplu: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (abonament ChatGPT)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Verifică / reîmprospătează / schimbă profilul
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Rulează agentul cu autentificare prin abonament
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## Workspace agent + skill-uri

Rădăcina workspace: `~/.quantclaw/workspace/` (configurabilă prin config).

Fișiere prompt injectate:
- `IDENTITY.md` — personalitatea și rolul agentului
- `USER.md` — contextul și preferințele utilizatorului
- `MEMORY.md` — fapte și lecții pe termen lung
- `AGENTS.md` — convenții de sesiune și reguli de inițializare
- `SOUL.md` — identitate de bază și principii operaționale

Skill-uri: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` sau `SKILL.toml`.

```bash
# Listează skill-urile instalate
quantclaw skills list

# Instalează din git
quantclaw skills install https://github.com/user/my-skill.git

# Audit de securitate înainte de instalare
quantclaw skills audit https://github.com/user/my-skill.git

# Elimină un skill
quantclaw skills remove my-skill
```

## Comenzi CLI

```bash
# Gestionarea workspace-ului
quantclaw onboard              # Asistent de configurare ghidată
quantclaw status               # Afișează starea daemon/agent
quantclaw doctor               # Rulează diagnostice de sistem

# Gateway + daemon
quantclaw gateway              # Pornește serverul gateway (127.0.0.1:42617)
quantclaw daemon               # Pornește runtime-ul autonom complet

# Agent
quantclaw agent                # Mod chat interactiv
quantclaw agent -m "message"   # Mod mesaj unic

# Gestionarea serviciilor
quantclaw service install      # Instalează ca serviciu OS (launchd/systemd)
quantclaw service start|stop|restart|status

# Canale
quantclaw channel list         # Listează canalele configurate
quantclaw channel doctor       # Verifică sănătatea canalelor
quantclaw channel bind-telegram 123456789

# Cron + programare
quantclaw cron list            # Listează sarcinile programate
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Memorie
quantclaw memory list          # Listează intrările de memorie
quantclaw memory get <key>     # Recuperează o memorie
quantclaw memory stats         # Statistici memorie

# Profiluri de autentificare
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Periferice hardware
quantclaw hardware discover    # Scanează dispozitivele conectate
quantclaw peripheral list      # Listează perifericele conectate
quantclaw peripheral flash     # Încarcă firmware pe dispozitiv

# Migrare
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Completări shell
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

Referință completă comenzi: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Cerințe preliminare

<details>
<summary><strong>Windows</strong></summary>

#### Necesare

1. **Visual Studio Build Tools** (furnizează linker-ul MSVC și Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    În timpul instalării (sau prin Visual Studio Installer), selectează sarcina de lucru **"Desktop development with C++"**.

2. **Rust toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    După instalare, deschide un terminal nou și rulează `rustup default stable` pentru a te asigura că toolchain-ul stabil este activ.

3. **Verifică** că ambele funcționează:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opțional

- **Docker Desktop** — necesar doar dacă folosești [runtime-ul Docker sandboxed](#suport-runtime-curent) (`runtime.kind = "docker"`). Instalează prin `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Necesare

1. **Build essentials:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Instalează Xcode Command Line Tools: `xcode-select --install`

2. **Rust toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Vezi [rustup.rs](https://rustup.rs) pentru detalii.

3. **Verifică** că ambele funcționează:
    ```bash
    rustc --version
    cargo --version
    ```

#### Instalator cu o singură linie

Sau sări peste pașii de mai sus și instalează totul (dependențe sistem, Rust, QuantClaw) cu o singură comandă:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### Cerințe de resurse pentru compilare

Construirea din sursă necesită mai multe resurse decât rularea binarului rezultat:

| Resursă        | Minimum | Recomandat  |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Disc liber** | 6 GB    | 10 GB+      |

Dacă gazda ta este sub minimum, folosește binare pre-construite:

```bash
./install.sh --prefer-prebuilt
```

Pentru a impune instalare doar cu binar, fără fallback sursă:

```bash
./install.sh --prebuilt-only
```

#### Opțional

- **Docker** — necesar doar dacă folosești [runtime-ul Docker sandboxed](#suport-runtime-curent) (`runtime.kind = "docker"`). Instalează prin managerul de pachete sau [docker.com](https://docs.docker.com/engine/install/).

> **Notă:** `cargo build --release` implicit folosește `codegen-units=1` pentru a reduce presiunea maximă de compilare. Pentru build-uri mai rapide pe mașini puternice, folosește `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Binare pre-construite

Resursele de lansare sunt publicate pentru:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Descarcă cele mai recente resurse de la:
<https://github.com/quant-speed/quantclaw/releases/latest>

## Documentație

Folosește-le când ai trecut de fluxul de onboarding și vrei referința mai detaliată.

- Începe cu [indexul documentației](docs/README.md) pentru navigare și „ce este unde."
- Citește [prezentarea arhitecturii](docs/architecture.md) pentru modelul complet al sistemului.
- Folosește [referința de configurare](docs/reference/api/config-reference.md) când ai nevoie de fiecare cheie și exemplu.
- Rulează Gateway-ul conform [runbook-ului operațional](docs/ops/operations-runbook.md).
- Urmează [QuantClaw Onboard](#pornire-rapidă) pentru configurare ghidată.
- Depanează eșecurile comune cu [ghidul de depanare](docs/ops/troubleshooting.md).
- Revizuiește [ghidul de securitate](docs/security/README.md) înainte de a expune ceva.

### Documentație de referință

- Hub documentație: [docs/README.md](docs/README.md)
- TOC documentație unificată: [docs/SUMMARY.md](docs/SUMMARY.md)
- Referință comenzi: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Referință configurare: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Referință providers: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Referință canale: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Runbook operațional: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Depanare: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Documentație de colaborare

- Ghid de contribuție: [CONTRIBUTING.md](CONTRIBUTING.md)
- Politica fluxului de lucru PR: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- Ghid flux de lucru CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Playbook recenzent: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Politica de divulgare a securității: [SECURITY.md](SECURITY.md)
- Șablon documentație: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Implementare + operațiuni

- Ghid de implementare în rețea: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Playbook proxy agent: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Ghiduri hardware: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw a fost construit pentru smooth crab 🦀, un asistent AI rapid și eficient. Construit de Argenis De La Rosa și comunitate.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## Susține QuantClaw

Dacă QuantClaw te ajută în muncă și vrei să susții dezvoltarea continuă, poți dona aici:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 Mulțumiri Speciale

Mulțumiri sincere comunităților și instituțiilor care inspiră și alimentează această muncă open-source:

- **Harvard University** — pentru cultivarea curiozității intelectuale și extinderea limitelor posibilului.
- **MIT** — pentru promovarea cunoștințelor deschise, open source și credința că tehnologia ar trebui să fie accesibilă tuturor.
- **Sundai Club** — pentru comunitate, energie și dorința neîncetată de a construi lucruri care contează.
- **Lumea și Dincolo** 🌍✨ — fiecărui contributor, visător și constructor care face din open source o forță a binelui. Aceasta este pentru voi.

Construim deschis pentru că cele mai bune idei vin de peste tot. Dacă citești asta, faci parte din asta. Bine ai venit. 🦀❤️

## Contribuție

Nou la QuantClaw? Caută probleme etichetate [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — vezi [Ghidul de Contribuție](CONTRIBUTING.md#first-time-contributors) pentru cum să începi. PR-urile create cu AI/vibe-coded sunt binevenite! 🤖

Vezi [CONTRIBUTING.md](CONTRIBUTING.md) și [CLA.md](docs/contributing/cla.md). Implementează un trait, trimite un PR:

- Ghid flux de lucru CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- `Provider` nou → `src/providers/`
- `Channel` nou → `src/channels/`
- `Observer` nou → `src/observability/`
- `Tool` nou → `src/tools/`
- `Memory` nou → `src/memory/`
- `Tunnel` nou → `src/tunnel/`
- `Peripheral` nou → `src/peripherals/`
- `Skill` nou → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Depozit Oficial & Avertisment de Uzurpare

**Acesta este singurul depozit oficial QuantClaw:**

> https://github.com/quant-speed/quantclaw

Orice alt depozit, organizație, domeniu sau pachet care pretinde a fi „QuantClaw" sau implică afiliere cu QuantClaw Labs este **neautorizat și nu este afiliat cu acest proiect**. Fork-urile neautorizate cunoscute vor fi listate în [TRADEMARK.md](docs/maintainers/trademark.md).

Dacă întâmpini uzurpare de identitate sau utilizare abuzivă a mărcii comerciale, te rugăm [deschide o problemă](https://github.com/quant-speed/quantclaw/issues).

---

## Licență

QuantClaw este dual-licențiat pentru deschidere maximă și protecția contributorilor:

| Licență | Caz de utilizare |
|---|---|
| [MIT](LICENSE-MIT) | Open-source, cercetare, academic, utilizare personală |
| [Apache 2.0](LICENSE-APACHE) | Protecție brevete, instituțional, implementare comercială |

Poți alege oricare licență. **Contributorii acordă automat drepturi sub ambele** — vezi [CLA.md](docs/contributing/cla.md) pentru acordul complet al contributorului.

### Marcă comercială

Numele și logo-ul **QuantClaw** sunt mărci comerciale ale QuantClaw Labs. Această licență nu acordă permisiunea de a le folosi pentru a implica aprobare sau afiliere. Vezi [TRADEMARK.md](docs/maintainers/trademark.md) pentru utilizări permise și interzise.

### Protecții pentru contributori

- **Păstrezi drepturile de autor** ale contribuțiilor tale
- **Acordarea de brevete** (Apache 2.0) te protejează de revendicări de brevete ale altor contributori
- Contribuțiile tale sunt **atribuite permanent** în istoricul commit-urilor și [NOTICE](NOTICE)
- Nu se transferă drepturi de marcă comercială prin contribuție

---

**QuantClaw** — Zero overhead. Zero compromisuri. Implementează oriunde. Schimbă orice. 🦀

## Contributori

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

Această listă este generată din graficul contributorilor GitHub și se actualizează automat.

## Istoricul Stelelor

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
