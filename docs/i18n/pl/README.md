<p align="center">
  <img src="https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/docs/assets/daemonclaw-banner.png" alt="DaemonClaw" width="600" />
</p>

<h1 align="center">🦀 DaemonClaw — Osobisty Asystent AI</h1>

<p align="center">
  <strong>Zero narzutu. Zero kompromisów. 100% Rust. 100% Agnostyczny.</strong><br>
  ⚡️ <strong>Działa na sprzęcie za $10 z <5MB RAM: To 99% mniej pamięci niż OpenClaw i 98% taniej niż Mac mini!</strong>
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
Stworzone przez studentów i członków społeczności Harvard, MIT i Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Języki:</strong>
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

DaemonClaw to osobisty asystent AI, który uruchamiasz na własnych urządzeniach. Odpowiada na kanałach, których już używasz (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work i więcej). Posiada panel webowy do kontroli w czasie rzeczywistym i może łączyć się z peryferiami sprzętowymi (ESP32, STM32, Arduino, Raspberry Pi). Gateway to tylko warstwa sterowania — produktem jest asystent.

Jeśli szukasz osobistego, jednoosobowego asystenta, który działa lokalnie, szybko i jest zawsze dostępny — to jest to.

<p align="center">
  <a href="https://daemonclawlabs.ai">Strona internetowa</a> ·
  <a href="docs/README.md">Dokumentacja</a> ·
  <a href="docs/architecture.md">Architektura</a> ·
  <a href="#szybki-start">Rozpocznij</a> ·
  <a href="#migracja-z-openclaw">Migracja z OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Rozwiązywanie problemów</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **Zalecana konfiguracja:** uruchom `daemonclaw onboard` w terminalu. DaemonClaw Onboard prowadzi Cię krok po kroku przez konfigurację gateway, workspace, kanałów i dostawcy. Jest to zalecana ścieżka konfiguracji i działa na macOS, Linux i Windows (przez WSL2). Nowa instalacja? Zacznij tutaj: [Rozpocznij](#szybki-start)

### Uwierzytelnianie subskrypcyjne (OAuth)

- **OpenAI Codex** (subskrypcja ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (klucz API lub token autoryzacyjny)

Uwaga dotycząca modeli: chociaż obsługiwanych jest wielu dostawców/modeli, dla najlepszego doświadczenia używaj najsilniejszego dostępnego modelu najnowszej generacji. Zobacz [Onboarding](#szybki-start).

Konfiguracja modeli + CLI: [Dokumentacja dostawców](docs/reference/api/providers-reference.md)
Rotacja profili autoryzacyjnych (OAuth vs klucze API) + failover: [Failover modeli](docs/reference/api/providers-reference.md)

## Instalacja (zalecana)

Środowisko uruchomieniowe: stabilny toolchain Rust. Pojedynczy plik binarny, brak zależności runtime.

### Homebrew (macOS/Linuxbrew)

```bash
brew install daemonclaw
```

### Instalacja jednym kliknięciem

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw
./install.sh
```

`daemonclaw onboard` uruchamia się automatycznie po instalacji, aby skonfigurować workspace i dostawcę.

## Szybki start (TL;DR)

Pełny przewodnik dla początkujących (autoryzacja, parowanie, kanały): [Rozpocznij](docs/setup-guides/one-click-bootstrap.md)

```bash
# Instalacja + onboarding
./install.sh --api-key "sk-..." --provider openrouter

# Uruchom gateway (serwer webhook + panel webowy)
daemonclaw gateway                # domyślnie: 127.0.0.1:42617
daemonclaw gateway --port 0       # losowy port (wzmocnione bezpieczeństwo)

# Porozmawiaj z asystentem
daemonclaw agent -m "Hello, DaemonClaw!"

# Tryb interaktywny
daemonclaw agent

# Uruchom pełne autonomiczne środowisko (gateway + kanały + cron + hands)
daemonclaw daemon

# Sprawdź status
daemonclaw status

# Uruchom diagnostykę
daemonclaw doctor
```

Aktualizujesz? Uruchom `daemonclaw doctor` po aktualizacji.

### Ze źródła (rozwój)

```bash
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw

cargo build --release --locked
cargo install --path . --force --locked

daemonclaw onboard
```

> **Tryb deweloperski (bez globalnej instalacji):** poprzedź komendy `cargo run --release --` (przykład: `cargo run --release -- status`).

## Migracja z OpenClaw

DaemonClaw może zaimportować Twój workspace, pamięć i konfigurację OpenClaw:

```bash
# Podgląd tego, co zostanie zmigrowane (bezpieczne, tylko odczyt)
daemonclaw migrate openclaw --dry-run

# Uruchom migrację
daemonclaw migrate openclaw
```

Migruje wpisy pamięci, pliki workspace i konfigurację z `~/.openclaw/` do `~/.daemonclaw/`. Konfiguracja jest automatycznie konwertowana z JSON do TOML.

## Domyślne ustawienia bezpieczeństwa (dostęp DM)

DaemonClaw łączy się z prawdziwymi platformami komunikacyjnymi. Traktuj przychodzące DM jako niezaufane dane wejściowe.

Pełny przewodnik bezpieczeństwa: [SECURITY.md](SECURITY.md)

Domyślne zachowanie na wszystkich kanałach:

- **Parowanie DM** (domyślne): nieznani nadawcy otrzymują krótki kod parowania i bot nie przetwarza ich wiadomości.
- Zatwierdź za pomocą: `daemonclaw pairing approve <channel> <code>` (wtedy nadawca jest dodawany do lokalnej listy dozwolonych).
- Publiczne przychodzące DM wymagają jawnej zgody w `config.toml`.
- Uruchom `daemonclaw doctor`, aby wykryć ryzykowne lub błędnie skonfigurowane polityki DM.

**Poziomy autonomii:**

| Poziom | Zachowanie |
|--------|------------|
| `ReadOnly` | Agent może obserwować, ale nie działać |
| `Supervised` (domyślny) | Agent działa z zatwierdzeniem dla operacji średniego/wysokiego ryzyka |
| `Full` | Agent działa autonomicznie w granicach polityki |

**Warstwy sandboxingu:** izolacja workspace, blokowanie przechodzenia ścieżek, lista dozwolonych poleceń, zabronione ścieżki (`/etc`, `/root`, `~/.ssh`), ograniczenie szybkości (maks. akcji/godzinę, limity kosztów/dzień).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Ogłoszenia

Użyj tej tablicy do ważnych ogłoszeń (zmiany łamiące, porady bezpieczeństwa, okna serwisowe i blokery wydań).

| Data (UTC) | Poziom       | Ogłoszenie                                                                                                                                                                                                                                                                                                                                                 | Działanie                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Krytyczny_  | **Nie jesteśmy powiązani** z `openagen/daemonclaw`, `daemonclaw.org` ani `daemonclaw.net`. Domeny `daemonclaw.org` i `daemonclaw.net` obecnie kierują do forka `openagen/daemonclaw`, a ta domena/repozytorium podszywają się pod naszą oficjalną stronę/projekt.                                                                                       | Nie ufaj informacjom, plikom binarnym, zbiórkom funduszy ani ogłoszeniom z tych źródeł. Używaj wyłącznie [tego repozytorium](https://github.com/DeliveryBoyTech/daemonclaw) i naszych zweryfikowanych kont społecznościowych.                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Ważny_ | Anthropic zaktualizował warunki uwierzytelniania i użytkowania poświadczeń 2026-02-19. Tokeny OAuth Claude Code (Free, Pro, Max) są przeznaczone wyłącznie dla Claude Code i Claude.ai; używanie tokenów OAuth z Claude Free/Pro/Max w jakimkolwiek innym produkcie, narzędziu lub usłudze (w tym Agent SDK) nie jest dozwolone i może naruszać Warunki korzystania z usługi. | Proszę tymczasowo unikać integracji OAuth Claude Code, aby zapobiec potencjalnym stratom. Oryginalna klauzula: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                    |

## Najważniejsze cechy

- **Lekkie środowisko uruchomieniowe domyślnie** — typowe workflow CLI i statusu działają w kopercie pamięci kilku megabajtów na buildach release.
- **Ekonomiczne wdrożenie** — zaprojektowane dla płytek za $10 i małych instancji chmurowych, bez ciężkich zależności runtime.
- **Szybki zimny start** — jednoplikowe środowisko Rust utrzymuje start komend i demona niemal natychmiastowy.
- **Przenośna architektura** — jeden plik binarny na ARM, x86 i RISC-V z wymiennymi dostawcami/kanałami/narzędziami.
- **Gateway lokalny** — pojedyncza warstwa sterowania dla sesji, kanałów, narzędzi, cron, SOP i zdarzeń.
- **Wielokanałowa skrzynka odbiorcza** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket i więcej.
- **Orkiestracja wielu agentów (Hands)** — autonomiczne roje agentów, które działają według harmonogramu i stają się inteligentniejsze z czasem.
- **Standardowe Procedury Operacyjne (SOP)** — automatyzacja workflow sterowana zdarzeniami z wyzwalaczami MQTT, webhook, cron i peryferiami.
- **Panel webowy** — interfejs React 19 + Vite z czatem w czasie rzeczywistym, przeglądarką pamięci, edytorem konfiguracji, menedżerem cron i inspektorem narzędzi.
- **Peryferia sprzętowe** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO przez trait `Peripheral`.
- **Narzędzia pierwszej klasy** — shell, plik I/O, przeglądarka, git, web fetch/search, MCP, Jira, Notion, Google Workspace i 70+ więcej.
- **Hooki cyklu życia** — przechwytuj i modyfikuj wywołania LLM, wykonania narzędzi i wiadomości na każdym etapie.
- **Platforma umiejętności** — wbudowane, społecznościowe i workspace skills z audytem bezpieczeństwa.
- **Obsługa tuneli** — Cloudflare, Tailscale, ngrok, OpenVPN i niestandardowe tunele do zdalnego dostępu.

### Dlaczego zespoły wybierają DaemonClaw

- **Lekki domyślnie:** mały plik binarny Rust, szybki start, niskie zużycie pamięci.
- **Bezpieczny z założenia:** parowanie, ścisły sandboxing, jawne listy dozwolonych, izolacja workspace.
- **W pełni wymienny:** podstawowe systemy to traity (dostawcy, kanały, narzędzia, pamięć, tunele).
- **Brak vendor lock-in:** obsługa dostawców kompatybilnych z OpenAI + podłączalne niestandardowe endpointy.

## Porównanie wydajności (DaemonClaw vs OpenClaw, odtwarzalne)

Szybki benchmark na maszynie lokalnej (macOS arm64, luty 2026) znormalizowany dla sprzętu edge 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | DaemonClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Język**                 | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Start (rdzeń 0.8GHz)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Rozmiar binarki**       | ~28MB (dist)  | N/A (Skrypty)  | ~8MB            | **~8.8 MB**          |
| **Koszt**                 | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Dowolny sprzęt $10** |

> Uwagi: Wyniki DaemonClaw są mierzone na buildach release przy użyciu `/usr/bin/time -l`. OpenClaw wymaga środowiska Node.js (typowo ~390MB dodatkowego narzutu pamięci), natomiast NanoBot wymaga środowiska Python. PicoClaw i DaemonClaw to statyczne pliki binarne. Powyższe wartości RAM dotyczą pamięci runtime; wymagania kompilacji są wyższe.

<p align="center">
  <img src="docs/assets/daemonclaw-comparison.jpeg" alt="DaemonClaw vs OpenClaw Comparison" width="800" />
</p>

### Odtwarzalny pomiar lokalny

```bash
cargo build --release
ls -lh target/release/daemonclaw

/usr/bin/time -l target/release/daemonclaw --help
/usr/bin/time -l target/release/daemonclaw status
```

## Wszystko, co do tej pory zbudowaliśmy

### Platforma podstawowa

- Gateway HTTP/WS/SSE warstwa sterowania z sesjami, obecnością, konfiguracją, cron, webhookami, panelem webowym i parowaniem.
- Interfejs CLI: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Pętla orkiestracji agenta z dispatchem narzędzi, konstrukcją promptów, klasyfikacją wiadomości i ładowaniem pamięci.
- Model sesji z egzekwowaniem polityki bezpieczeństwa, poziomami autonomii i bramkowaniem zatwierdzeń.
- Odporny wrapper dostawcy z failoverem, ponawianiem i routingiem modeli na 20+ backendach LLM.

### Kanały

Kanały: WhatsApp (natywny), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Za bramkami feature: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Panel webowy

Panel webowy React 19 + Vite 6 + Tailwind CSS 4 serwowany bezpośrednio z Gateway:

- **Dashboard** — przegląd systemu, status zdrowia, uptime, śledzenie kosztów
- **Czat z agentem** — interaktywny czat z agentem
- **Pamięć** — przeglądanie i zarządzanie wpisami pamięci
- **Konfiguracja** — podgląd i edycja konfiguracji
- **Cron** — zarządzanie zaplanowanymi zadaniami
- **Narzędzia** — przeglądanie dostępnych narzędzi
- **Logi** — podgląd logów aktywności agenta
- **Koszty** — użycie tokenów i śledzenie kosztów
- **Doctor** — diagnostyka zdrowia systemu
- **Integracje** — status i konfiguracja integracji
- **Parowanie** — zarządzanie parowaniem urządzeń

### Cele firmware

| Cel | Platforma | Przeznaczenie |
|-----|-----------|---------------|
| ESP32 | Espressif ESP32 | Bezprzewodowy agent peryferyjny |
| ESP32-UI | ESP32 + Wyświetlacz | Agent z interfejsem wizualnym |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Peryferia przemysłowe |
| Arduino | Arduino | Podstawowy mostek czujników/aktuatorów |
| Uno Q Bridge | Arduino Uno | Mostek szeregowy do agenta |

### Narzędzia + automatyzacja

- **Podstawowe:** shell, odczyt/zapis/edycja plików, operacje git, wyszukiwanie glob, wyszukiwanie treści
- **Web:** sterowanie przeglądarką, web fetch, wyszukiwanie web, zrzut ekranu, info o obrazie, odczyt PDF
- **Integracje:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** wrapper narzędzi Model Context Protocol + odroczone zestawy narzędzi
- **Planowanie:** cron add/remove/update/run, narzędzie planowania
- **Pamięć:** recall, store, forget, knowledge, project intel
- **Zaawansowane:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **Sprzęt:** board info, memory map, memory read (za bramką feature)

### Środowisko uruchomieniowe + bezpieczeństwo

- **Poziomy autonomii:** ReadOnly, Supervised (domyślny), Full.
- **Sandboxing:** izolacja workspace, blokowanie przechodzenia ścieżek, listy dozwolonych poleceń, zabronione ścieżki, Landlock (Linux), Bubblewrap.
- **Ograniczenie szybkości:** maks. akcji na godzinę, maks. koszt na dzień (konfigurowalne).
- **Bramkowanie zatwierdzeń:** interaktywne zatwierdzanie operacji średniego/wysokiego ryzyka.
- **E-stop:** możliwość awaryjnego wyłączenia.
- **129+ testów bezpieczeństwa** w automatycznym CI.

### Operacje + pakowanie

- Panel webowy serwowany bezpośrednio z Gateway.
- Obsługa tuneli: Cloudflare, Tailscale, ngrok, OpenVPN, niestandardowe polecenie.
- Adapter runtime Docker do konteneryzowanego wykonywania.
- CI/CD: beta (auto na push) → stable (ręczny dispatch) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Gotowe pliki binarne dla Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Konfiguracja

Minimalna `~/.daemonclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Pełna dokumentacja konfiguracji: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Konfiguracja kanałów

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

### Konfiguracja tunelu

```toml
[tunnel]
kind = "cloudflare"  # lub "tailscale", "ngrok", "openvpn", "custom", "none"
```

Szczegóły: [Dokumentacja kanałów](docs/reference/api/channels-reference.md) · [Dokumentacja konfiguracji](docs/reference/api/config-reference.md)

### Obsługa runtime (aktualnie)

- **`native`** (domyślny) — bezpośrednie wykonywanie procesów, najszybsza ścieżka, idealne dla zaufanych środowisk.
- **`docker`** — pełna izolacja kontenerowa, wymuszone polityki bezpieczeństwa, wymaga Docker.

Ustaw `runtime.kind = "docker"` dla ścisłego sandboxingu lub izolacji sieciowej.

## Uwierzytelnianie subskrypcyjne (OpenAI Codex / Claude Code / Gemini)

DaemonClaw obsługuje natywne profile autoryzacyjne subskrypcji (wiele kont, szyfrowanie w spoczynku).

- Plik przechowywania: `~/.daemonclaw/auth-profiles.json`
- Klucz szyfrowania: `~/.daemonclaw/.secret_key`
- Format ID profilu: `<provider>:<profile_name>` (przykład: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (subskrypcja ChatGPT)
daemonclaw auth login --provider openai-codex --device-code

# Gemini OAuth
daemonclaw auth login --provider gemini --profile default

# Anthropic setup-token
daemonclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Sprawdź / odśwież / przełącz profil
daemonclaw auth status
daemonclaw auth refresh --provider openai-codex --profile default
daemonclaw auth use --provider openai-codex --profile work

# Uruchom agenta z autoryzacją subskrypcji
daemonclaw agent --provider openai-codex -m "hello"
daemonclaw agent --provider anthropic -m "hello"
```

## Workspace agenta + umiejętności

Katalog główny workspace: `~/.daemonclaw/workspace/` (konfigurowalne przez config).

Wstrzykiwane pliki promptów:
- `IDENTITY.md` — osobowość i rola agenta
- `USER.md` — kontekst i preferencje użytkownika
- `MEMORY.md` — długoterminowe fakty i lekcje
- `AGENTS.md` — konwencje sesji i reguły inicjalizacji
- `SOUL.md` — podstawowa tożsamość i zasady działania

Umiejętności: `~/.daemonclaw/workspace/skills/<skill>/SKILL.md` lub `SKILL.toml`.

```bash
# Lista zainstalowanych umiejętności
daemonclaw skills list

# Instalacja z git
daemonclaw skills install https://github.com/user/my-skill.git

# Audyt bezpieczeństwa przed instalacją
daemonclaw skills audit https://github.com/user/my-skill.git

# Usuń umiejętność
daemonclaw skills remove my-skill
```

## Komendy CLI

```bash
# Zarządzanie workspace
daemonclaw onboard              # Kreator konfiguracji z przewodnikiem
daemonclaw status               # Pokaż status demona/agenta
daemonclaw doctor               # Uruchom diagnostykę systemu

# Gateway + demon
daemonclaw gateway              # Uruchom serwer gateway (127.0.0.1:42617)
daemonclaw daemon               # Uruchom pełne autonomiczne środowisko

# Agent
daemonclaw agent                # Tryb interaktywnego czatu
daemonclaw agent -m "message"   # Tryb pojedynczej wiadomości

# Zarządzanie usługami
daemonclaw service install      # Zainstaluj jako usługę OS (launchd/systemd)
daemonclaw service start|stop|restart|status

# Kanały
daemonclaw channel list         # Lista skonfigurowanych kanałów
daemonclaw channel doctor       # Sprawdź zdrowie kanałów
daemonclaw channel bind-telegram 123456789

# Cron + planowanie
daemonclaw cron list            # Lista zaplanowanych zadań
daemonclaw cron add "*/5 * * * *" --prompt "Check system health"
daemonclaw cron remove <id>

# Pamięć
daemonclaw memory list          # Lista wpisów pamięci
daemonclaw memory get <key>     # Pobierz wspomnienie
daemonclaw memory stats         # Statystyki pamięci

# Profile autoryzacyjne
daemonclaw auth login --provider <name>
daemonclaw auth status
daemonclaw auth use --provider <name> --profile <profile>

# Peryferia sprzętowe
daemonclaw hardware discover    # Skanuj podłączone urządzenia
daemonclaw peripheral list      # Lista podłączonych peryferiów
daemonclaw peripheral flash     # Flash firmware na urządzenie

# Migracja
daemonclaw migrate openclaw --dry-run
daemonclaw migrate openclaw

# Uzupełnianie powłoki
source <(daemonclaw completions bash)
daemonclaw completions zsh > ~/.zfunc/_daemonclaw
```

Pełna dokumentacja komend: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Wymagania wstępne

<details>
<summary><strong>Windows</strong></summary>

#### Wymagane

1. **Visual Studio Build Tools** (zapewnia linker MSVC i Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Podczas instalacji (lub przez Visual Studio Installer) wybierz workload **"Desktop development with C++"**.

2. **Toolchain Rust:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Po instalacji otwórz nowy terminal i uruchom `rustup default stable`, aby upewnić się, że aktywny jest stabilny toolchain.

3. **Sprawdź**, czy oba działają:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opcjonalne

- **Docker Desktop** — wymagany tylko przy użyciu [runtime Docker z sandboxem](#obsługa-runtime-aktualnie) (`runtime.kind = "docker"`). Zainstaluj przez `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Wymagane

1. **Narzędzia budowania:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Zainstaluj Xcode Command Line Tools: `xcode-select --install`

2. **Toolchain Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Zobacz [rustup.rs](https://rustup.rs) po szczegóły.

3. **Sprawdź**, czy oba działają:
    ```bash
    rustc --version
    cargo --version
    ```

#### Instalator jednoliniowy

Lub pomiń powyższe kroki i zainstaluj wszystko (zależności systemowe, Rust, DaemonClaw) jednym poleceniem:

```bash
curl -LsSf https://raw.githubusercontent.com/DeliveryBoyTech/daemonclaw/master/install.sh | bash
```

#### Wymagania zasobów kompilacji

Budowanie ze źródła wymaga więcej zasobów niż uruchamianie wynikowego pliku binarnego:

| Zasób          | Minimum | Zalecane    |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Wolne miejsce** | 6 GB | 10 GB+      |

Jeśli Twój host jest poniżej minimum, użyj gotowych plików binarnych:

```bash
./install.sh --prefer-prebuilt
```

Aby wymusić instalację wyłącznie z pliku binarnego, bez fallbacku na źródło:

```bash
./install.sh --prebuilt-only
```

#### Opcjonalne

- **Docker** — wymagany tylko przy użyciu [runtime Docker z sandboxem](#obsługa-runtime-aktualnie) (`runtime.kind = "docker"`). Zainstaluj przez menedżer pakietów lub [docker.com](https://docs.docker.com/engine/install/).

> **Uwaga:** Domyślny `cargo build --release` używa `codegen-units=1`, aby obniżyć szczytowe obciążenie kompilacji. Dla szybszych buildów na mocnych maszynach użyj `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Gotowe pliki binarne

Zasoby wydań są publikowane dla:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Pobierz najnowsze zasoby z:
<https://github.com/DeliveryBoyTech/daemonclaw/releases/latest>

## Dokumentacja

Używaj tych, gdy przeszedłeś już przez onboarding i chcesz głębszej dokumentacji.

- Zacznij od [indeksu dokumentacji](docs/README.md), aby zobaczyć nawigację i „co gdzie jest."
- Przeczytaj [przegląd architektury](docs/architecture.md), aby poznać pełny model systemu.
- Użyj [dokumentacji konfiguracji](docs/reference/api/config-reference.md), gdy potrzebujesz każdego klucza i przykładu.
- Uruchom Gateway zgodnie z [podręcznikiem operacyjnym](docs/ops/operations-runbook.md).
- Postępuj zgodnie z [DaemonClaw Onboard](#szybki-start) dla konfiguracji z przewodnikiem.
- Debuguj typowe awarie z [przewodnikiem rozwiązywania problemów](docs/ops/troubleshooting.md).
- Przejrzyj [wskazówki bezpieczeństwa](docs/security/README.md) przed wystawieniem czegokolwiek.

### Dokumentacja referencyjna

- Centrum dokumentacji: [docs/README.md](docs/README.md)
- Ujednolicony spis treści: [docs/SUMMARY.md](docs/SUMMARY.md)
- Dokumentacja komend: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Dokumentacja konfiguracji: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Dokumentacja dostawców: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Dokumentacja kanałów: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Podręcznik operacyjny: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Rozwiązywanie problemów: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Dokumentacja współpracy

- Przewodnik kontrybutora: [CONTRIBUTING.md](CONTRIBUTING.md)
- Polityka workflow PR: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- Przewodnik workflow CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Podręcznik recenzenta: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Polityka ujawniania bezpieczeństwa: [SECURITY.md](SECURITY.md)
- Szablon dokumentacji: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Wdrożenie + operacje

- Przewodnik wdrożenia sieciowego: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Podręcznik agenta proxy: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Przewodniki sprzętowe: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

DaemonClaw został zbudowany dla smooth crab 🦀, szybkiego i wydajnego asystenta AI. Stworzony przez Argenisa De La Rosę i społeczność.

- [daemonclawlabs.ai](https://daemonclawlabs.ai)
- [@daemonclawlabs](https://x.com/daemonclawlabs)

## Wesprzyj DaemonClaw

### 🙏 Specjalne podziękowania

Serdeczne podziękowania dla społeczności i instytucji, które inspirują i napędzają tę pracę open-source:

- **Harvard University** — za wspieranie ciekawości intelektualnej i przesuwanie granic tego, co możliwe.
- **MIT** — za promowanie otwartej wiedzy, open source i przekonania, że technologia powinna być dostępna dla wszystkich.
- **Sundai Club** — za społeczność, energię i nieustanny zapał do budowania rzeczy, które mają znaczenie.
- **Świat i dalej** 🌍✨ — dla każdego kontrybutora, marzyciela i twórcy, który sprawia, że open source jest siłą dobra. To dla Ciebie.

Budujemy w otwartości, ponieważ najlepsze pomysły pochodzą zewsząd. Jeśli to czytasz, jesteś tego częścią. Witaj. 🦀❤️

## Współtworzenie

Nowy w DaemonClaw? Szukaj issues oznaczonych [`good first issue`](https://github.com/DeliveryBoyTech/daemonclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — zobacz nasz [Przewodnik kontrybutora](CONTRIBUTING.md#first-time-contributors), aby dowiedzieć się jak zacząć. PR-y z AI/vibe-coded mile widziane! 🤖

Zobacz [CONTRIBUTING.md](CONTRIBUTING.md) i [CLA.md](docs/contributing/cla.md). Zaimplementuj trait, wyślij PR:

- Przewodnik workflow CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Nowy `Provider` → `src/providers/`
- Nowy `Channel` → `src/channels/`
- Nowy `Observer` → `src/observability/`
- Nowy `Tool` → `src/tools/`
- Nowy `Memory` → `src/memory/`
- Nowy `Tunnel` → `src/tunnel/`
- Nowy `Peripheral` → `src/peripherals/`
- Nowy `Skill` → `~/.daemonclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Oficjalne repozytorium i ostrzeżenie przed podszywaniem się

**To jest jedyne oficjalne repozytorium DaemonClaw:**

> https://github.com/DeliveryBoyTech/daemonclaw

Każde inne repozytorium, organizacja, domena lub pakiet twierdzący, że jest "DaemonClaw" lub sugerujący powiązanie z DaemonClaw Labs jest **nieautoryzowany i niepowiązany z tym projektem**. Znane nieautoryzowane forki będą wymienione w [TRADEMARK.md](docs/maintainers/trademark.md).

Jeśli napotkasz podszywanie się lub nadużycie znaku towarowego, proszę [otwórz zgłoszenie](https://github.com/DeliveryBoyTech/daemonclaw/issues).

---

## Licencja

DaemonClaw jest podwójnie licencjonowany dla maksymalnej otwartości i ochrony kontrybutorów:

| Licencja | Przypadek użycia |
|----------|------------------|
| [MIT](LICENSE-MIT) | Open-source, badania, akademia, użytek osobisty |
| [Apache 2.0](LICENSE-APACHE) | Ochrona patentowa, instytucjonalne, wdrożenia komercyjne |

Możesz wybrać dowolną licencję. **Kontrybutorzy automatycznie udzielają praw na obie** — zobacz [CLA.md](docs/contributing/cla.md) po pełną umowę kontrybutora.

### Znak towarowy

Nazwa **DaemonClaw** i logo są znakami towarowymi DaemonClaw Labs. Ta licencja nie udziela pozwolenia na ich używanie w celu sugerowania poparcia lub powiązania. Zobacz [TRADEMARK.md](docs/maintainers/trademark.md) po dozwolone i zabronione użycia.

### Ochrona kontrybutorów

- **Zachowujesz prawa autorskie** do swoich wkładów
- **Udzielenie patentu** (Apache 2.0) chroni Cię przed roszczeniami patentowymi innych kontrybutorów
- Twoje wkłady są **trwale przypisane** w historii commitów i [NOTICE](NOTICE)
- Żadne prawa do znaku towarowego nie są przenoszone przez współtworzenie

---

**DaemonClaw** — Zero narzutu. Zero kompromisów. Wdrażaj wszędzie. Wymieniaj wszystko. 🦀

## Kontrybutorzy

<a href="https://github.com/DeliveryBoyTech/daemonclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=DeliveryBoyTech/daemonclaw" alt="DaemonClaw contributors" />
</a>

Ta lista jest generowana z grafu kontrybutorów GitHub i aktualizuje się automatycznie.

## Historia gwiazdek

<p align="center">
  <a href="https://www.star-history.com/#DeliveryBoyTech/daemonclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=DeliveryBoyTech/daemonclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
