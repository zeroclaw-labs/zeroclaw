<p align="center">
  <img src="https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/docs/assets/daemonclaw-banner.png" alt="DaemonClaw" width="600" />
</p>

<h1 align="center">🦀 DaemonClaw — Szemelyes MI Asszisztens</h1>

<p align="center">
  <strong>Nulla terheles. Nulla kompromisszum. 100% Rust. 100% Agnosztikus.</strong><br>
  ⚡️ <strong>$10-os hardveren fut <5MB RAM-mal: Ez 99%-kal kevesebb memoria, mint az OpenClaw es 98%-kal olcsobb, mint egy Mac mini!</strong>
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
A Harvard, MIT es Sundai.Club kozossegek diakjai es tagjai epitettek.
</p>

<p align="center">
  🌐 <strong>Nyelvek:</strong>
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

A DaemonClaw egy szemelyes MI asszisztens, amelyet a sajat eszkozeiden futtathatsz. Valaszol a mar hasznalt csatornaidon (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work es meg tobb). Rendelkezik webes vezerlopulttal valos ideju iranyitashoz, es csatlakoztathat hardver periferiakhoz (ESP32, STM32, Arduino, Raspberry Pi). A Gateway csupan a vezerlesi sik — a termek maga az asszisztens.

Ha szemelyes, egyfelhasznalos asszisztenst szeretnel, ami lokalis, gyors es mindig elerheto, ez az.

<p align="center">
  <a href="https://daemonclawlabs.ai">Weboldal</a> ·
  <a href="docs/README.md">Dokumentacio</a> ·
  <a href="docs/architecture.md">Architektura</a> ·
  <a href="#gyors-inditas-tldr">Kezdes</a> ·
  <a href="#atallas-openclawrol">Atallas OpenClawrol</a> ·
  <a href="docs/ops/troubleshooting.md">Hibaelharitas</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **Ajanlott beallitas:** futtasd a `daemonclaw onboard` parancsot a terminalban. A DaemonClaw Onboard lepesrol lepesre vegigvezet a gateway, munkater, csatornak es szolgaltato beallitasan. Ez az ajanlott beallitasi ut, es mukodik macOS-en, Linuxon es Windowson (WSL2-n keresztul). Uj telepites? Kezdd itt: [Kezdes](#gyors-inditas-tldr)

### Elofizetes hitelesites (OAuth)

- **OpenAI Codex** (ChatGPT elofizetes)
- **Gemini** (Google OAuth)
- **Anthropic** (API kulcs vagy hitelesitesi token)

Modell megjegyzes: bar sok szolgaltato/modell tamogatott, a legjobb elmeny erdekeben hasznald a legerosebb, legujabb generacios modellt. Lasd [Onboarding](#gyors-inditas-tldr).

Modellek konfiguracio + CLI: [Szolgaltatoi referencia](docs/reference/api/providers-reference.md)
Auth profil rotacio (OAuth vs API kulcsok) + failover: [Modell failover](docs/reference/api/providers-reference.md)

## Telepites (ajanlott)

Futtato kornyezet: Rust stable toolchain. Egyetlen binaris, nincs futtatasi ideju fuggoseg.

### Homebrew (macOS/Linuxbrew)

```bash
brew install daemonclaw
```

### Egy kattintasos telepites

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw
./install.sh
```

A `daemonclaw onboard` automatikusan lefut a telepites utan a munkater es szolgaltato konfiguralasakor.

## Gyors inditas (TL;DR)

Teljes kezdo utmutato (hitelesites, parositas, csatornak): [Kezdes](docs/setup-guides/one-click-bootstrap.md)

```bash
# Telepites + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Gateway inditasa (webhook szerver + webes vezerlopult)
daemonclaw gateway                # alapertelmezett: 127.0.0.1:42617
daemonclaw gateway --port 0       # veletlenszeru port (biztonsagi szilarditas)

# Beszelgess az asszisztenssel
daemonclaw agent -m "Hello, DaemonClaw!"

# Interaktiv mod
daemonclaw agent

# Teljes autonom futtatas inditasa (gateway + csatornak + cron + hands)
daemonclaw daemon

# Allapot ellenorzes
daemonclaw status

# Diagnosztika futtatasa
daemonclaw doctor
```

Frissites? Futtasd a `daemonclaw doctor` parancsot a frissites utan.

### Forrasbol (fejlesztes)

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw

cargo build --release --locked
cargo install --path . --force --locked

daemonclaw onboard
```

> **Fejlesztoi alternativa (globalis telepites nelkul):** a parancsokat prefixeld `cargo run --release --`-vel (pelda: `cargo run --release -- status`).

## Atallas OpenClawrol

A DaemonClaw importalhatja az OpenClaw munkateret, memoriat es konfiguraciot:

```bash
# Elonezet az attelepitendo adatokrol (biztonsagos, csak olvasható)
daemonclaw migrate openclaw --dry-run

# Migracio futtatasa
daemonclaw migrate openclaw
```

Ez migralja a memoriabejegyzeseket, munkater fajlokat es konfiguraciot a `~/.openclaw/` konyvtarbol a `~/.daemonclaw/` konyvtarba. A konfiguracio automatikusan JSON-bol TOML-ra konvertalodik.

## Biztonsagi alapertelmezesek (DM hozzaferes)

A DaemonClaw valos uzenetfeluletekkez csatlakozik. Kezeld a bejovo DM-eket nem megbizhato bemenetekkent.

Teljes biztonsagi utmutato: [SECURITY.md](SECURITY.md)

Alapertelmezett viselkedes minden csatornan:

- **DM parositas** (alapertelmezett): az ismeretlen feladok rovid parosito kodot kapnak, es a bot nem dolgozza fel az uzenetuket.
- Jovahagy paranccsal: `daemonclaw pairing approve <channel> <code>` (ezutan a felado felkerul egy lokalis engedelyezesi listara).
- A nyilvanos bejovo DM-ek kifejezett opt-in-t igenyelnek a `config.toml`-ban.
- Futtasd a `daemonclaw doctor` parancsot a kockazatos vagy rosszul konfiguralt DM szabalyzatok feltarasahoz.

**Autonomia szintek:**

| Szint | Viselkedes |
|-------|------------|
| `ReadOnly` | Az agens megfigyel, de nem cselekszik |
| `Supervised` (alapertelmezett) | Az agens jovahagyassal cselekszik kozepes/magas kockazatu muveletenel |
| `Full` | Az agens autonoman cselekszik a szabalyzat hataran belul |

**Sandboxing retegek:** munkater izolalas, utvonal-atjaras blokkolas, parancs engedelyezesi listak, tiltott utvonalak (`/etc`, `/root`, `~/.ssh`), sebessegkorlatozas (max muveletek/ora, koltseg/nap korlatok).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Kozlemenyek

Hasznald ezt a tablat fontos ertesitesekhez (torekenyen kompatibilis valtozasok, biztonsagi tanacsadok, karbantartasi idosavok es kiadasi blokkolok).

| Datum (UTC) | Szint | Ertesites | Teendo |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Kritikus_ | **Nem** allunk kapcsolatban az `openagen/daemonclaw`, `daemonclaw.org` vagy `daemonclaw.net` oldalakkal. A `daemonclaw.org` es `daemonclaw.net` domainek jelenleg az `openagen/daemonclaw` fork-ra mutatnak, es az a domain/tarolo megszemelyesiti a hivatalos weboldalunkat/projektunket. | Ne bizz meg az ezekbol a forrasokbol szarmazo informaciokban, binarisokban, adomanygyujtesekben vagy kozlemenyekben. Kizarolag [ezt a tarolot](https://github.com/DeliveryBoyTech/daemonclaw) es az ellenorzott kozossegi media fiokjainkat hasznald. |
| 2026-02-19 | _Fontos_ | Az Anthropic frissitette a Hitelesitesi es Hitellevelek Hasznalara vonatkozo felteteleket 2026-02-19-en. A Claude Code OAuth tokenek (Free, Pro, Max) kizarolag a Claude Code es a Claude.ai szamara keszultek; az OAuth tokenek barmely mas termekben, eszkozben vagy szolgaltatasban valo hasznalata (beleertve az Agent SDK-t) nem megengedett es sertheti a Fogyasztoi Szolgaltatasi Felteteleket. | Kerlek ideiglenesen keruld a Claude Code OAuth integraciokat a potencialis veszteseg megelozese erdekeben. Eredeti kikotes: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## Fobb jellemzok

- **Konnyu futtatokornyezet alapertelmezetten** — a szokasos CLI es allapot munkafolyamatok nehany megabajtos memoria burkban futnak release buildekben.
- **Koltseghatekony telepites** — $10-os kartyakhoz es kis cloud peldanyokhoz tervezve, nehez futtatokornyezeti fuggosegek nelkul.
- **Gyors hideg inditas** — az egyetlen binarisbol allo Rust futtatokornyezet szinte azonnali parancs- es daemon-inditast biztosit.
- **Hordozhato architektura** — egy binaris ARM, x86 es RISC-V rendszereken cserelheto szolgaltatok/csatornak/eszkozokkel.
- **Lokalis-eloszor Gateway** — egyetlen vezerlesi sik a munkamenetekhez, csatornakhoz, eszkozokhoz, cron-hoz, SOP-khoz es esemenyekhez.
- **Tobbcsatornas beerkeze** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket es meg tobb.
- **Tobbagens orkesztracio (Hands)** — autonom agens rajok, amelyek utemezetten futnak es idovel okosabbak lesznek.
- **Szabvanyos Muveleti Eljarasok (SOPs)** — esemenyvezeerlt munkafolyamat automatizalas MQTT, webhook, cron es periferia triggerekkel.
- **Webes vezerlopult** — React 19 + Vite webes felulet valos ideju csevegeessel, memoriaboongeszevel, konfiguracioszerkesztovel, cron kezelovel es eszkoz vizsgaloval.
- **Hardver periferiak** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO a `Peripheral` trait-en keresztul.
- **Elso osztalyu eszkozok** — shell, file I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace es 70+ tovabb.
- **Eletciklus hookok** — LLM hivasok, eszkozvegrehajtasok es uzenetek elfogasa es modositasa minden szinten.
- **Kepesseg platform** — beepitett, kozossegi es munkater kepessegek biztonsagi auditalassal.
- **Tunnel tamogatas** — Cloudflare, Tailscale, ngrok, OpenVPN es egyedi tunnelek tavoli hozzafereshez.

### Miert valasztjak a csapatok a DaemonClaw-t

- **Konnyu alapertelmezetten:** kis Rust binaris, gyors inditas, alacsony memoriahasznalat.
- **Biztonsagos tervezessel:** parositas, szigoru sandboxing, kifejezett engedelyezesi listak, munkater hatarolás.
- **Teljesen cserelheto:** az alaprendszerek trait-ek (providers, channels, tools, memory, tunnels).
- **Nincs bezartsag:** OpenAI-kompatibilis szolgaltatoi tamogatas + csatlakoztatható egyedi vegpontok.

## Benchmark pillanatkep (DaemonClaw vs OpenClaw, Reprodukalhato)

Lokalis gepi gyors benchmark (macOS arm64, 2026 feb.) normalizalva 0.8GHz edge hardverre.

|                           | OpenClaw      | NanoBot        | PicoClaw        | DaemonClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Nyelv**                 | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Inditas (0.8GHz core)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Binaris meret**         | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Koltseg**               | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Barmilyen hardver $10** |

> Megjegyzesek: A DaemonClaw eredmenyek release buildeken merve `/usr/bin/time -l` hasznalataval. Az OpenClaw Node.js futtatokornyezetet igenyel (tipikusan ~390MB memoria terheles), mig a NanoBot Python futtatokornyezetet. A PicoClaw es DaemonClaw statikus binarisok. A fenti RAM adatok futtatasi ideju memoriat mutatnak; a forditasi ideju kovetelmenyek magasabbak.

<p align="center">
  <img src="docs/assets/daemonclaw-comparison.jpeg" alt="DaemonClaw vs OpenClaw Comparison" width="800" />
</p>

### Reprodukalhato lokalis meres

```bash
cargo build --release
ls -lh target/release/daemonclaw

/usr/bin/time -l target/release/daemonclaw --help
/usr/bin/time -l target/release/daemonclaw status
```

## Minden, amit eddig epitettunk

### Alapplatform

- Gateway HTTP/WS/SSE vezerlesi sik munkamenetekkel, jelenleettel, konfiguracioval, cron-nal, webhookkal, webes vezerlopulttal es parositassal.
- CLI felulet: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Agens orkesztracios hurk eszkoz-kuldessel, prompt epitessel, uzenet osztalyozassal es memoria betoltessel.
- Munkamenet modell biztonsagi szabalyzat ervenyesitessel, autonomia szintekkel es jovahagyasi kapuval.
- Ellenallo szolgaltatoi wrapper failover-rel, ujraprobalassal es modell iranyitassal 20+ LLM backend-en.

### Csatornak

Csatornak: WhatsApp (native), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Feature-gated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Webes vezerlopult

React 19 + Vite 6 + Tailwind CSS 4 webes vezerlopult, amelyet kozvetlenul a Gateway szolgaltat ki:

- **Dashboard** — rendszer attekintes, egeszsegi allapot, uzemido, koltsegkovetes
- **Agent Chat** — interaktiv csevegees az agenssel
- **Memory** — memoriabejegyzesek bongeszese es kezelese
- **Config** — konfiguracio megtekintese es szerkesztese
- **Cron** — utemezett feladatok kezelese
- **Tools** — elerheto eszkozok bongeszese
- **Logs** — agens tevekenysegnaplo megtekintese
- **Cost** — token hasznalat es koltsegkovetes
- **Doctor** — rendszer egeszseugyi diagnosztika
- **Integrations** — integracios allapot es beallitas
- **Pairing** — eszkoz parositas kezeles

### Firmware celok

| Cel | Platform | Rendeltetees |
|-----|----------|-------------|
| ESP32 | Espressif ESP32 | Vezetek nelkuli periferia agens |
| ESP32-UI | ESP32 + Display | Agens vizualis feluelettel |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Ipari periferia |
| Arduino | Arduino | Alap szenzor/aktualtor hid |
| Uno Q Bridge | Arduino Uno | Soros hid az agenshez |

### Eszkozok + automatizalas

- **Alap:** shell, file read/write/edit, git operations, glob search, content search
- **Web:** browser control, web fetch, web search, screenshot, image info, PDF read
- **Integraciok:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + deferred tool sets
- **Utemezes:** cron add/remove/update/run, schedule tool
- **Memoria:** recall, store, forget, knowledge, project intel
- **Halado:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **Hardver:** board info, memory map, memory read (feature-gated)

### Futtatokornyezet + biztonsag

- **Autonomia szintek:** ReadOnly, Supervised (alapertelmezett), Full.
- **Sandboxing:** munkater izolalas, utvonal-atjaras blokkolas, parancs engedelyezesi listak, tiltott utvonalak, Landlock (Linux), Bubblewrap.
- **Sebessegkorlatozas:** max muveletek orankent, max koltseg naponta (konfiguralhato).
- **Jovahagyasi kapu:** interaktiv jovahagy kozepes/magas kockazatu mueveletekhez.
- **E-stop:** veszleallitasi kepesseg.
- **129+ biztonsagi teszt** automatizalt CI-ben.

### Muveletek + csomagolas

- Webes vezerlopult kozvetlenul a Gateway-bol kiszolgalva.
- Tunnel tamogatas: Cloudflare, Tailscale, ngrok, OpenVPN, egyedi parancs.
- Docker runtime adapter konterizalt vegrehajtashoz.
- CI/CD: beta (auto on push) → stable (manual dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Elore elkeszitett binarisok Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64) rendszerekhez.


## Konfiguracio

Minimalis `~/.daemonclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Teljes konfiguracios referencia: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Csatorna konfiguracio

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

### Tunnel konfiguracio

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

Reszletek: [Csatorna referencia](docs/reference/api/channels-reference.md) · [Konfiguracios referencia](docs/reference/api/config-reference.md)

### Futtatokornyezet tamogatas (aktualis)

- **`native`** (alapertelmezett) — kozvetlen folyamat vegrehajtas, leggyorsabb ut, idealis megbizhato kornyezetekhez.
- **`docker`** — teljes kontener izolalas, ervenyesitett biztonsagi szabalyzatok, Docker szukseges.

Allitsd be a `runtime.kind = "docker"` erteket a szigoru sandboxinghoz vagy halozati izolaciohoz.

## Elofizetes hitelesites (OpenAI Codex / Claude Code / Gemini)

A DaemonClaw tamogatja az elofizetes-nativ hitelesitesi profilokat (tobb fiok, titkositva tarolva).

- Tarolo fajl: `~/.daemonclaw/auth-profiles.json`
- Titkositasi kulcs: `~/.daemonclaw/.secret_key`
- Profil azonosito formatum: `<provider>:<profile_name>` (pelda: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT subscription)
daemonclaw auth login --provider openai-codex --device-code

# Gemini OAuth
daemonclaw auth login --provider gemini --profile default

# Anthropic setup-token
daemonclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Check / refresh / switch profile
daemonclaw auth status
daemonclaw auth refresh --provider openai-codex --profile default
daemonclaw auth use --provider openai-codex --profile work

# Run the agent with subscription auth
daemonclaw agent --provider openai-codex -m "hello"
daemonclaw agent --provider anthropic -m "hello"
```

## Agens munkater + kepessegek

Munkater gyoker: `~/.daemonclaw/workspace/` (konfiguralhato a config-on keresztul).

Beinjektalt prompt fajlok:
- `IDENTITY.md` — agens szemelyiseg es szerep
- `USER.md` — felhasznaloi kontextus es prefernciak
- `MEMORY.md` — hosszu tavu tenyek es tanulsagok
- `AGENTS.md` — munkamenet konvenciok es inicializalasi szabalyok
- `SOUL.md` — alapveto identitas es mukodesi elvek

Kepessegek: `~/.daemonclaw/workspace/skills/<skill>/SKILL.md` vagy `SKILL.toml`.

```bash
# List installed skills
daemonclaw skills list

# Install from git
daemonclaw skills install https://github.com/user/my-skill.git

# Security audit before install
daemonclaw skills audit https://github.com/user/my-skill.git

# Remove a skill
daemonclaw skills remove my-skill
```

## CLI parancsok

```bash
# Munkater kezeles
daemonclaw onboard              # Vezerelt beallitasi varazslo
daemonclaw status               # Daemon/agent allapot megjelenites
daemonclaw doctor               # Rendszer diagnosztika futtatasa

# Gateway + daemon
daemonclaw gateway              # Gateway szerver inditasa (127.0.0.1:42617)
daemonclaw daemon               # Teljes autonom futtatas inditasa

# Agens
daemonclaw agent                # Interaktiv csevegesi mod
daemonclaw agent -m "message"   # Egyszeri uzenet mod

# Szolgaltatas kezeles
daemonclaw service install      # Telepites OS szolgaltataskent (launchd/systemd)
daemonclaw service start|stop|restart|status

# Csatornak
daemonclaw channel list         # Konfiguralt csatornak listazasa
daemonclaw channel doctor       # Csatorna egeszseg ellenorzes
daemonclaw channel bind-telegram 123456789

# Cron + utemezes
daemonclaw cron list            # Utemezett feladatok listazasa
daemonclaw cron add "*/5 * * * *" --prompt "Check system health"
daemonclaw cron remove <id>

# Memoria
daemonclaw memory list          # Memoriabejegyzesek listazasa
daemonclaw memory get <key>     # Memoria lekerese
daemonclaw memory stats         # Memoria statisztikak

# Hitelesitesi profilok
daemonclaw auth login --provider <name>
daemonclaw auth status
daemonclaw auth use --provider <name> --profile <profile>

# Hardver periferiak
daemonclaw hardware discover    # Csatlakoztatott eszkozok keresese
daemonclaw peripheral list      # Csatlakoztatott periferiak listazasa
daemonclaw peripheral flash     # Firmware felirasa eszkozre

# Migracio
daemonclaw migrate openclaw --dry-run
daemonclaw migrate openclaw

# Shell kiegeszitesek
source <(daemonclaw completions bash)
daemonclaw completions zsh > ~/.zfunc/_daemonclaw
```

Teljes parancs referencia: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Elofeltetelek

<details>
<summary><strong>Windows</strong></summary>

#### Szukseges

1. **Visual Studio Build Tools** (biztositja az MSVC linkert es a Windows SDK-t):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    A telepites soran (vagy a Visual Studio Installer-en keresztul) valaszd a **"Desktop development with C++"** munkafolyamatot.

2. **Rust toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    A telepites utan nyiss egy uj terminalt es futtasd a `rustup default stable` parancsot a stabil toolchain aktivalasahoz.

3. **Ellenorzes**, hogy mindketto mukodik:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opcionalis

- **Docker Desktop** — csak a [Docker sandboxed runtime](#futtatokornyezet-tamogatas-aktualis) hasznalatahoz szukseges (`runtime.kind = "docker"`). Telepites: `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Szukseges

1. **Epitesi alapeszkozok:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Telepitsd az Xcode Command Line Tools-t: `xcode-select --install`

2. **Rust toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Reszletekert lasd [rustup.rs](https://rustup.rs).

3. **Ellenorzes**, hogy mindketto mukodik:
    ```bash
    rustc --version
    cargo --version
    ```

#### Egyvonalas telepito

Vagy hagyd ki a fenti lepeseket es telepits mindent (rendszer fuggosegek, Rust, DaemonClaw) egyetlen paranccsal:

```bash
curl -LsSf https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/install.sh | bash
```

#### Forditasi eroforrasigeny

A forrasbol valo epites tobb eroforras igenyel, mint az eredmeny binaris futtatasa:

| Eroforras | Minimum | Ajanlott |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Szabad lemez** | 6 GB    | 10 GB+      |

Ha a gazdageped a minimum alatt van, hasznalj elore elkeszitett binarisokat:

```bash
./install.sh --prefer-prebuilt
```

Kizarolag binaris telepiteshez forras alternativa nelkul:

```bash
./install.sh --prebuilt-only
```

#### Opcionalis

- **Docker** — csak a [Docker sandboxed runtime](#futtatokornyezet-tamogatas-aktualis) hasznalatahoz szukseges (`runtime.kind = "docker"`). Telepites a csomagkezelodon keresztul vagy [docker.com](https://docs.docker.com/engine/install/).

> **Megjegyzes:** Az alapertelmezett `cargo build --release` `codegen-units=1` erteket hasznal a csucs forditasi terheles csokkenteseere. Gyorsabb epitesekhez eros gepeken hasznald a `cargo build --profile release-fast` parancsot.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Elore elkeszitett binarisok

Kiadas eszkozok az alabbi platformokra kerulnek kozetetelre:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Toltsd le a legujabb eszkozoket innen:
<https://github.com/DeliveryBoyTech/daemonclaw/releases/latest>

## Dokumentacio

Hasznald ezeket, ha tuljutottal az onboarding folyamaton es melyebb referenciara van szukseged.

- Kezdd a [dokumentacios indexszel](docs/README.md) a navigaciohoz es a "mi hol talalhato" informaciohoz.
- Olvasd el az [architektura attekintest](docs/architecture.md) a teljes rendszermodellhez.
- Hasznald a [konfiguracios referenciat](docs/reference/api/config-reference.md), ha minden kulcsra es peldara szukseged van.
- Futtasd a Gateway-t a konyv szerint az [uzemeltetesi kezikonyvvel](docs/ops/operations-runbook.md).
- Kovesd a [DaemonClaw Onboard](#gyors-inditas-tldr) szolgaltatast a vezerelt beallitashoz.
- Hibakeress a gyakori problemakat a [hibaelharitasi utmutatoval](docs/ops/troubleshooting.md).
- Tekintsd at a [biztonsagi utmutatast](docs/security/README.md) mielott barmit is kiteszel.

### Referencia dokumentaciok

- Dokumentacios kozpont: [docs/README.md](docs/README.md)
- Egysegesitett tartalomjegyzek: [docs/SUMMARY.md](docs/SUMMARY.md)
- Parancs referencia: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Konfiguracios referencia: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Szolgaltatoi referencia: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Csatorna referencia: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Uzemeltetesi kezikonyv: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Hibaelharitas: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Egyuttmukodesi dokumentaciok

- Hozzajarulasi utmutato: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR munkafolyamat szabalyzat: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI munkafolyamat utmutato: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Biraloi kezikonyv: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Biztonsagi kozzeteeteli szabalyzat: [SECURITY.md](SECURITY.md)
- Dokumentacios sablon: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Telepites + muveletek

- Halozati telepitesi utmutato: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Proxy agens kezikonyv: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Hardver utmutatok: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

A DaemonClaw a smooth crab 🦀 szamara keszult, egy gyors es hatekony MI asszisztens. Epitette Argenis De La Rosa es a kozosseg.

- [daemonclawlabs.ai](https://daemonclawlabs.ai)
- [@daemonclawlabs](https://x.com/daemonclawlabs)

## Tamogasd a DaemonClaw-t

### 🙏 Kulonos koszonet

Szivbol jovo koszonet a kozossegeknek es intezmenyeknek, amelyek inspiraljak es taplaljak ezt a nyilt forrasu munkat:

- **Harvard University** — az intellektualis kivancsiság apolasaert es a lehetosegek hatarainak tolásáert.
- **MIT** — a nyilt tudas, nyilt forras es azon hit bajnokakent, hogy a technologianak mindenki szamara elerheto kell lennie.
- **Sundai Club** — a kozossegert, az energiaert es a szuntelen torekveseert, hogy fontos dolgokat epitsenek.
- **A Vilag es Azon Tul** 🌍✨ — minden hozzajarulonak, almodonak es epitonek, aki a nyilt forrast a jo erdekeben mukodo erove teszi. Ez neked szol.

Nyiltan epitunk, mert a legjobb otletek mindenhonnan jonnek. Ha ezt olvasod, a resze vagy. Udvozlunk. 🦀❤️

## Hozzajarulas

Uj vagy a DaemonClaw-ban? Keresd a [`good first issue`](https://github.com/DeliveryBoyTech/daemonclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) cimkevel ellatott issue-kat — lasd a [Hozzajarulasi utmutatot](CONTRIBUTING.md#first-time-contributors) a kezdeshez. AI/vibe-coded PR-ok szivesen latottak! 🤖

Lasd [CONTRIBUTING.md](CONTRIBUTING.md) es [CLA.md](docs/contributing/cla.md). Implementalj egy trait-et, kuuldj be egy PR-t:

- CI munkafolyamat utmutato: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Uj `Provider` → `src/providers/`
- Uj `Channel` → `src/channels/`
- Uj `Observer` → `src/observability/`
- Uj `Tool` → `src/tools/`
- Uj `Memory` → `src/memory/`
- Uj `Tunnel` → `src/tunnel/`
- Uj `Peripheral` → `src/peripherals/`
- Uj `Skill` → `~/.daemonclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Hivatalos tarolo es megszemelyesitesi figyelmeztetes

**Ez az egyetlen hivatalos DaemonClaw tarolo:**

> https://github.com/DeliveryBoyTech/daemonclaw

Barmely mas tarolo, szervezet, domain vagy csomag, amely azt allitja, hogy "DaemonClaw" vagy kapcsolatot sugall a DaemonClaw Labs-szal, **jogosulatlan es nem all kapcsolatban ezzel a projekttel**. Az ismert jogosulatlan forkok a [TRADEMARK.md](docs/maintainers/trademark.md) fajlban lesznek felsorolva.

Ha megszemelyesitessel vagy vedjeggyel valo visszaelessel talalkozol, kerlek [nyiss egy issue-t](https://github.com/DeliveryBoyTech/daemonclaw/issues).

---

## Licenc

A DaemonClaw kettos licenccel rendelkezik a maximalis nyitottsag es hozzajaruloi vedelem erdekeben:

| Licenc | Felhasznalasi eset |
|---|---|
| [MIT](LICENSE-MIT) | Nyilt forras, kutatas, akademiai, szemelyes haszanalat |
| [Apache 2.0](LICENSE-APACHE) | Szabadalmi vedelem, intezmenyi, kereskedelmi telepites |

Barmely licencet valaszthatod. **A hozzajarulok automatikusan mindketto alatt jogot biztositanak** — lasd [CLA.md](docs/contributing/cla.md) a teljes hozzajarulasi megallapodasert.

### Vedjegy

A **DaemonClaw** nev es logo a DaemonClaw Labs vedjegyei. Ez a licenc nem ad engedelyt arra, hogy tamogatast vagy kapcsolatot sugalljanak. Lasd [TRADEMARK.md](docs/maintainers/trademark.md) a megengedett es tiltott hasznalati modokert.

### Hozzajaruloi vedelmek

- **Megtartod a szerzoi jogot** a hozzajarulasaidon
- **Szabadalmi engedely** (Apache 2.0) vedi meg mas hozzajarulok szabadalmi igenyeitol
- A hozzajarulasaid **veglegesen attribulaltak** a commit tortenelben es a [NOTICE](NOTICE) fajlban
- Nem kerulnek at vedjegyjogok a hozzajarulassal

---

**DaemonClaw** — Nulla terheles. Nulla kompromisszum. Telepites barhova. Csere barmire. 🦀

## Hozzajarulok

<a href="https://github.com/DeliveryBoyTech/daemonclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=DeliveryBoyTech/daemonclaw" alt="DaemonClaw contributors" />
</a>

Ez a lista a GitHub hozzajaruloi grafikonjabol keszul es automatikusan frissul.

## Csillag tortenelem

<p align="center">
  <a href="https://www.star-history.com/#DeliveryBoyTech/daemonclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
