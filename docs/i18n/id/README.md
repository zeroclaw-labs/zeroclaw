<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — Asisten AI Pribadi</h1>

<p align="center">
  <strong>Nol overhead. Nol kompromi. 100% Rust. 100% Agnostik.</strong><br>
  ⚡️ <strong>Berjalan di perangkat keras $10 dengan RAM <5MB: Itu 99% lebih hemat memori dari OpenClaw dan 98% lebih murah dari Mac mini!</strong>
</p>

<p align="center">
Dibangun oleh mahasiswa dan anggota komunitas Harvard, MIT, dan Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Bahasa:</strong>
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

QuantClaw adalah asisten AI pribadi yang Anda jalankan di perangkat sendiri. Ia menjawab Anda melalui saluran yang sudah Anda gunakan (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, dan lainnya). Ia memiliki dasbor web untuk kontrol real-time dan dapat terhubung ke periferal perangkat keras (ESP32, STM32, Arduino, Raspberry Pi). Gateway hanyalah bidang kendali — produknya adalah asisten.

Jika Anda menginginkan asisten pribadi, pengguna tunggal, yang terasa lokal, cepat, dan selalu aktif, inilah solusinya.

<p align="center">
  <a href="https://quantspeed.ai">Situs Web</a> ·
  <a href="docs/README.md">Dokumentasi</a> ·
  <a href="docs/architecture.md">Arsitektur</a> ·
  <a href="#mulai-cepat">Memulai</a> ·
  <a href="#migrasi-dari-openclaw">Migrasi dari OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">Pemecahan Masalah</a> ·
</p>

> **Pengaturan yang disarankan:** jalankan `quantclaw onboard` di terminal Anda. QuantClaw Onboard memandu Anda langkah demi langkah dalam menyiapkan gateway, workspace, saluran, dan provider. Ini adalah jalur pengaturan yang disarankan dan berfungsi di macOS, Linux, dan Windows (melalui WSL2). Instalasi baru? Mulai di sini: [Memulai](#mulai-cepat)

### Autentikasi Berlangganan (OAuth)

- **OpenAI Codex** (langganan ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (kunci API atau token autentikasi)

Catatan model: meskipun banyak provider/model didukung, untuk pengalaman terbaik gunakan model generasi terbaru terkuat yang tersedia untuk Anda. Lihat [Onboarding](#mulai-cepat).

Konfigurasi model + CLI: [Referensi Provider](docs/reference/api/providers-reference.md)
Rotasi profil autentikasi (OAuth vs kunci API) + failover: [Failover Model](docs/reference/api/providers-reference.md)

## Instal (disarankan)

Runtime: Rust stable toolchain. Biner tunggal, tanpa dependensi runtime.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### Bootstrap sekali klik

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` berjalan otomatis setelah instalasi untuk mengonfigurasi workspace dan provider Anda.

## Mulai cepat (TL;DR)

Panduan lengkap pemula (autentikasi, pairing, saluran): [Memulai](docs/setup-guides/one-click-bootstrap.md)

```bash
# Instal + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Mulai gateway (server webhook + dasbor web)
quantclaw gateway                # default: 127.0.0.1:42617
quantclaw gateway --port 0       # port acak (keamanan ditingkatkan)

# Bicara ke asisten
quantclaw agent -m "Hello, QuantClaw!"

# Mode interaktif
quantclaw agent

# Mulai runtime otonom penuh (gateway + saluran + cron + hands)
quantclaw daemon

# Periksa status
quantclaw status

# Jalankan diagnostik
quantclaw doctor
```

Memperbarui? Jalankan `quantclaw doctor` setelah pembaruan.

### Dari sumber (pengembangan)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **Alternatif dev (tanpa instalasi global):** awali perintah dengan `cargo run --release --` (contoh: `cargo run --release -- status`).

## Migrasi dari OpenClaw

QuantClaw dapat mengimpor workspace, memori, dan konfigurasi OpenClaw Anda:

```bash
# Pratinjau apa yang akan dimigrasikan (aman, hanya-baca)
quantclaw migrate openclaw --dry-run

# Jalankan migrasi
quantclaw migrate openclaw
```

Ini memigrasikan entri memori, file workspace, dan konfigurasi Anda dari `~/.openclaw/` ke `~/.quantclaw/`. Konfigurasi dikonversi dari JSON ke TOML secara otomatis.

## Default keamanan (akses DM)

QuantClaw terhubung ke permukaan pesan nyata. Perlakukan DM masuk sebagai input tidak tepercaya.

Panduan keamanan lengkap: [SECURITY.md](SECURITY.md)

Perilaku default di semua saluran:

- **Pairing DM** (default): pengirim yang tidak dikenal menerima kode pairing singkat dan bot tidak memproses pesan mereka.
- Setujui dengan: `quantclaw pairing approve <channel> <code>` (kemudian pengirim ditambahkan ke daftar izin lokal).
- DM masuk publik memerlukan opt-in eksplisit di `config.toml`.
- Jalankan `quantclaw doctor` untuk menemukan kebijakan DM yang berisiko atau salah konfigurasi.

**Level otonomi:**

| Level | Perilaku |
|-------|----------|
| `ReadOnly` | Agen dapat mengamati tetapi tidak bertindak |
| `Supervised` (default) | Agen bertindak dengan persetujuan untuk operasi risiko menengah/tinggi |
| `Full` | Agen bertindak secara otonom dalam batas kebijakan |

**Lapisan sandboxing:** isolasi workspace, pemblokiran traversal jalur, daftar izin perintah, jalur terlarang (`/etc`, `/root`, `~/.ssh`), pembatasan laju (maksimum tindakan/jam, batas biaya/hari).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 Pengumuman

Gunakan papan ini untuk pemberitahuan penting (perubahan yang merusak, saran keamanan, jendela pemeliharaan, dan pemblokir rilis).

| Tanggal (UTC) | Level       | Pemberitahuan                                                                                                                                                                                                                                                                                                                                                 | Tindakan                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _Kritis_  | Kami **tidak berafiliasi** dengan `openagen/quantclaw`, `quantclaw.org` atau `quantclaw.net`. Domain `quantclaw.org` dan `quantclaw.net` saat ini mengarah ke fork `openagen/quantclaw`, dan domain/repositori tersebut menyamar sebagai situs web/proyek resmi kami.                                                                                       | Jangan percaya informasi, biner, penggalangan dana, atau pengumuman dari sumber tersebut. Gunakan hanya [repositori ini](https://github.com/quant-speed/quantclaw) dan akun sosial terverifikasi kami.                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-02-19 | _Penting_ | Anthropic memperbarui ketentuan Autentikasi dan Penggunaan Kredensial pada 2026-02-19. Token OAuth Claude Code (Free, Pro, Max) ditujukan secara eksklusif untuk Claude Code dan Claude.ai; menggunakan token OAuth dari Claude Free/Pro/Max di produk, alat, atau layanan lain (termasuk Agent SDK) tidak diizinkan dan dapat melanggar Ketentuan Layanan Konsumen. | Harap sementara hindari integrasi OAuth Claude Code untuk mencegah potensi kerugian. Klausul asli: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use).                                                                                                                                                                                                                                                                                                                                                                                    |

## Sorotan

- **Runtime Ringan secara Default** — alur kerja CLI dan status umum berjalan dalam amplop memori beberapa megabyte pada build rilis.
- **Deployment Hemat Biaya** — dirancang untuk board $10 dan instans cloud kecil, tanpa dependensi runtime berat.
- **Cold Start Cepat** — runtime Rust biner tunggal menjaga startup perintah dan daemon hampir instan.
- **Arsitektur Portabel** — satu biner di ARM, x86, dan RISC-V dengan provider/saluran/alat yang dapat ditukar.
- **Gateway Lokal-Pertama** — bidang kendali tunggal untuk sesi, saluran, alat, cron, SOP, dan peristiwa.
- **Inbox multi-saluran** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket, dan lainnya.
- **Orkestrasi multi-agen (Hands)** — swarm agen otonom yang berjalan sesuai jadwal dan semakin pintar seiring waktu.
- **Standard Operating Procedures (SOP)** — otomasi alur kerja berbasis peristiwa dengan MQTT, webhook, cron, dan pemicu periferal.
- **Dasbor Web** — UI web React 19 + Vite dengan obrolan real-time, browser memori, editor konfigurasi, manajer cron, dan inspektor alat.
- **Periferal perangkat keras** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO melalui trait `Peripheral`.
- **Alat kelas satu** — shell, file I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace, dan 70+ lainnya.
- **Hook siklus hidup** — intersep dan modifikasi panggilan LLM, eksekusi alat, dan pesan di setiap tahap.
- **Platform skill** — skill bawaan, komunitas, dan workspace dengan audit keamanan.
- **Dukungan tunnel** — Cloudflare, Tailscale, ngrok, OpenVPN, dan tunnel kustom untuk akses jarak jauh.

### Mengapa tim memilih QuantClaw

- **Ringan secara default:** biner Rust kecil, startup cepat, jejak memori rendah.
- **Aman secara desain:** pairing, sandboxing ketat, daftar izin eksplisit, pelingkupan workspace.
- **Sepenuhnya dapat ditukar:** sistem inti adalah trait (provider, saluran, alat, memori, tunnel).
- **Tanpa lock-in:** dukungan provider kompatibel OpenAI + endpoint kustom pluggable.

## Cuplikan Benchmark (QuantClaw vs OpenClaw, Dapat Direproduksi)

Benchmark cepat mesin lokal (macOS arm64, Feb 2026) dinormalisasi untuk perangkat keras edge 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **Bahasa**                | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **Startup (inti 0.8GHz)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **Ukuran Biner**          | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **Biaya**                 | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Perangkat keras apa pun $10** |

> Catatan: Hasil QuantClaw diukur pada build rilis menggunakan `/usr/bin/time -l`. OpenClaw memerlukan runtime Node.js (biasanya ~390MB overhead memori tambahan), sedangkan NanoBot memerlukan runtime Python. PicoClaw dan QuantClaw adalah biner statis. Angka RAM di atas adalah memori runtime; kebutuhan kompilasi saat build lebih tinggi.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### Pengukuran lokal yang dapat direproduksi

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## Semua yang telah kami bangun sejauh ini

### Platform inti

- Bidang kendali HTTP/WS/SSE Gateway dengan sesi, presence, konfigurasi, cron, webhook, dasbor web, dan pairing.
- Permukaan CLI: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- Loop orkestrasi agen dengan dispatch alat, konstruksi prompt, klasifikasi pesan, dan pemuatan memori.
- Model sesi dengan penegakan kebijakan keamanan, level otonomi, dan gating persetujuan.
- Wrapper provider resilient dengan failover, retry, dan routing model di 20+ backend LLM.

### Saluran

Saluran: WhatsApp (native), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

Feature-gated: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### Dasbor web

Dasbor web React 19 + Vite 6 + Tailwind CSS 4 yang disajikan langsung dari Gateway:

- **Dashboard** — ikhtisar sistem, status kesehatan, uptime, pelacakan biaya
- **Agent Chat** — obrolan interaktif dengan agen
- **Memory** — jelajahi dan kelola entri memori
- **Config** — lihat dan edit konfigurasi
- **Cron** — kelola tugas terjadwal
- **Tools** — jelajahi alat yang tersedia
- **Logs** — lihat log aktivitas agen
- **Cost** — penggunaan token dan pelacakan biaya
- **Doctor** — diagnostik kesehatan sistem
- **Integrations** — status integrasi dan pengaturan
- **Pairing** — manajemen pairing perangkat

### Target firmware

| Target | Platform | Tujuan |
|--------|----------|--------|
| ESP32 | Espressif ESP32 | Agen periferal nirkabel |
| ESP32-UI | ESP32 + Display | Agen dengan antarmuka visual |
| STM32 Nucleo | STM32 (ARM Cortex-M) | Periferal industri |
| Arduino | Arduino | Jembatan sensor/aktuator dasar |
| Uno Q Bridge | Arduino Uno | Jembatan serial ke agen |

### Alat + otomasi

- **Inti:** shell, file read/write/edit, operasi git, glob search, content search
- **Web:** browser control, web fetch, web search, screenshot, image info, PDF read
- **Integrasi:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + deferred tool sets
- **Penjadwalan:** cron add/remove/update/run, schedule tool
- **Memori:** recall, store, forget, knowledge, project intel
- **Lanjutan:** delegate (agen-ke-agen), swarm, model switch/routing, security ops, cloud ops
- **Perangkat keras:** board info, memory map, memory read (feature-gated)

### Runtime + keamanan

- **Level otonomi:** ReadOnly, Supervised (default), Full.
- **Sandboxing:** isolasi workspace, pemblokiran traversal jalur, daftar izin perintah, jalur terlarang, Landlock (Linux), Bubblewrap.
- **Pembatasan laju:** maksimum tindakan per jam, maksimum biaya per hari (dapat dikonfigurasi).
- **Gating persetujuan:** persetujuan interaktif untuk operasi risiko menengah/tinggi.
- **E-stop:** kemampuan shutdown darurat.
- **129+ tes keamanan** dalam CI otomatis.

### Ops + pengemasan

- Dasbor web disajikan langsung dari Gateway.
- Dukungan tunnel: Cloudflare, Tailscale, ngrok, OpenVPN, perintah kustom.
- Adapter runtime Docker untuk eksekusi terkontainerisasi.
- CI/CD: beta (otomatis saat push) → stable (dispatch manual) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Biner pre-built untuk Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## Konfigurasi

Minimal `~/.quantclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

Referensi konfigurasi lengkap: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### Konfigurasi saluran

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

### Konfigurasi tunnel

```toml
[tunnel]
kind = "cloudflare"  # atau "tailscale", "ngrok", "openvpn", "custom", "none"
```

Detail: [Referensi Saluran](docs/reference/api/channels-reference.md) · [Referensi Konfigurasi](docs/reference/api/config-reference.md)

### Dukungan runtime (saat ini)

- **`native`** (default) — eksekusi proses langsung, jalur tercepat, ideal untuk lingkungan tepercaya.
- **`docker`** — isolasi kontainer penuh, kebijakan keamanan ditegakkan, memerlukan Docker.

Atur `runtime.kind = "docker"` untuk sandboxing ketat atau isolasi jaringan.

## Autentikasi Berlangganan (OpenAI Codex / Claude Code / Gemini)

QuantClaw mendukung profil autentikasi native berlangganan (multi-akun, terenkripsi saat istirahat).

- File penyimpanan: `~/.quantclaw/auth-profiles.json`
- Kunci enkripsi: `~/.quantclaw/.secret_key`
- Format id profil: `<provider>:<profile_name>` (contoh: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (langganan ChatGPT)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Periksa / refresh / ganti profil
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Jalankan agen dengan auth berlangganan
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## Workspace agen + skill

Root workspace: `~/.quantclaw/workspace/` (dapat dikonfigurasi melalui config).

File prompt yang diinjeksi:
- `IDENTITY.md` — kepribadian dan peran agen
- `USER.md` — konteks dan preferensi pengguna
- `MEMORY.md` — fakta dan pelajaran jangka panjang
- `AGENTS.md` — konvensi sesi dan aturan inisialisasi
- `SOUL.md` — identitas inti dan prinsip operasi

Skill: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` atau `SKILL.toml`.

```bash
# Daftar skill yang terinstal
quantclaw skills list

# Instal dari git
quantclaw skills install https://github.com/user/my-skill.git

# Audit keamanan sebelum instalasi
quantclaw skills audit https://github.com/user/my-skill.git

# Hapus skill
quantclaw skills remove my-skill
```

## Perintah CLI

```bash
# Manajemen workspace
quantclaw onboard              # Wizard pengaturan terpandu
quantclaw status               # Tampilkan status daemon/agen
quantclaw doctor               # Jalankan diagnostik sistem

# Gateway + daemon
quantclaw gateway              # Mulai server gateway (127.0.0.1:42617)
quantclaw daemon               # Mulai runtime otonom penuh

# Agen
quantclaw agent                # Mode obrolan interaktif
quantclaw agent -m "message"   # Mode pesan tunggal

# Manajemen layanan
quantclaw service install      # Instal sebagai layanan OS (launchd/systemd)
quantclaw service start|stop|restart|status

# Saluran
quantclaw channel list         # Daftar saluran yang dikonfigurasi
quantclaw channel doctor       # Periksa kesehatan saluran
quantclaw channel bind-telegram 123456789

# Cron + penjadwalan
quantclaw cron list            # Daftar tugas terjadwal
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Memori
quantclaw memory list          # Daftar entri memori
quantclaw memory get <key>     # Ambil memori
quantclaw memory stats         # Statistik memori

# Profil autentikasi
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Periferal perangkat keras
quantclaw hardware discover    # Pindai perangkat yang terhubung
quantclaw peripheral list      # Daftar periferal yang terhubung
quantclaw peripheral flash     # Flash firmware ke perangkat

# Migrasi
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Pelengkapan shell
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

Referensi perintah lengkap: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## Prasyarat

<details>
<summary><strong>Windows</strong></summary>

#### Diperlukan

1. **Visual Studio Build Tools** (menyediakan linker MSVC dan Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    Selama instalasi (atau melalui Visual Studio Installer), pilih beban kerja **"Desktop development with C++"**.

2. **Rust toolchain:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    Setelah instalasi, buka terminal baru dan jalankan `rustup default stable` untuk memastikan toolchain stabil aktif.

3. **Verifikasi** keduanya berfungsi:
    ```powershell
    rustc --version
    cargo --version
    ```

#### Opsional

- **Docker Desktop** — diperlukan hanya jika menggunakan [runtime Docker sandboxed](#dukungan-runtime-saat-ini) (`runtime.kind = "docker"`). Instal melalui `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### Diperlukan

1. **Build essentials:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Instal Xcode Command Line Tools: `xcode-select --install`

2. **Rust toolchain:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    Lihat [rustup.rs](https://rustup.rs) untuk detail.

3. **Verifikasi** keduanya berfungsi:
    ```bash
    rustc --version
    cargo --version
    ```

#### Installer Satu Baris

Atau lewati langkah di atas dan instal semuanya (dependensi sistem, Rust, QuantClaw) dalam satu perintah:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### Kebutuhan sumber daya kompilasi

Membangun dari sumber memerlukan lebih banyak sumber daya daripada menjalankan biner yang dihasilkan:

| Sumber Daya    | Minimum | Disarankan  |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **Disk kosong**| 6 GB    | 10 GB+      |

Jika host Anda di bawah minimum, gunakan biner pre-built:

```bash
./install.sh --prefer-prebuilt
```

Untuk memerlukan instalasi hanya-biner tanpa fallback sumber:

```bash
./install.sh --prebuilt-only
```

#### Opsional

- **Docker** — diperlukan hanya jika menggunakan [runtime Docker sandboxed](#dukungan-runtime-saat-ini) (`runtime.kind = "docker"`). Instal melalui manajer paket Anda atau [docker.com](https://docs.docker.com/engine/install/).

> **Catatan:** Default `cargo build --release` menggunakan `codegen-units=1` untuk menurunkan tekanan kompilasi puncak. Untuk build lebih cepat di mesin yang kuat, gunakan `cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### Biner pre-built

Aset rilis dipublikasikan untuk:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

Unduh aset terbaru dari:
<https://github.com/quant-speed/quantclaw/releases/latest>

## Dokumentasi

Gunakan ini ketika Anda sudah melewati alur onboarding dan menginginkan referensi yang lebih mendalam.

- Mulai dengan [indeks dokumentasi](docs/README.md) untuk navigasi dan "apa di mana."
- Baca [ikhtisar arsitektur](docs/architecture.md) untuk model sistem lengkap.
- Gunakan [referensi konfigurasi](docs/reference/api/config-reference.md) ketika Anda memerlukan setiap kunci dan contoh.
- Jalankan Gateway sesuai buku dengan [runbook operasional](docs/ops/operations-runbook.md).
- Ikuti [QuantClaw Onboard](#mulai-cepat) untuk pengaturan terpandu.
- Debug kegagalan umum dengan [panduan pemecahan masalah](docs/ops/troubleshooting.md).
- Tinjau [panduan keamanan](docs/security/README.md) sebelum mengekspos apa pun.

### Dokumentasi referensi

- Hub dokumentasi: [docs/README.md](docs/README.md)
- TOC dokumentasi terpadu: [docs/SUMMARY.md](docs/SUMMARY.md)
- Referensi perintah: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- Referensi konfigurasi: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- Referensi provider: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- Referensi saluran: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- Runbook operasional: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- Pemecahan masalah: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### Dokumentasi kolaborasi

- Panduan kontribusi: [CONTRIBUTING.md](CONTRIBUTING.md)
- Kebijakan alur kerja PR: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- Panduan alur kerja CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- Playbook reviewer: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- Kebijakan pengungkapan keamanan: [SECURITY.md](SECURITY.md)
- Template dokumentasi: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### Deployment + operasi

- Panduan deployment jaringan: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- Playbook proxy agent: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- Panduan perangkat keras: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw dibangun untuk smooth crab 🦀, asisten AI yang cepat dan efisien. Dibangun oleh Argenis De La Rosa dan komunitas.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## Dukung QuantClaw

Jika QuantClaw membantu pekerjaan Anda dan Anda ingin mendukung pengembangan berkelanjutan, Anda dapat berdonasi di sini:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 Terima Kasih Khusus

Terima kasih yang tulus kepada komunitas dan institusi yang menginspirasi dan mendorong pekerjaan open-source ini:

- **Harvard University** — untuk memupuk rasa ingin tahu intelektual dan mendorong batas dari apa yang mungkin.
- **MIT** — untuk memperjuangkan pengetahuan terbuka, open source, dan keyakinan bahwa teknologi harus dapat diakses oleh semua orang.
- **Sundai Club** — untuk komunitas, energi, dan dorongan tanpa henti untuk membangun hal-hal yang penting.
- **Dunia & Seterusnya** 🌍✨ — kepada setiap kontributor, pemimpi, dan pembangun di luar sana yang menjadikan open source sebagai kekuatan untuk kebaikan. Ini untuk kalian.

Kami membangun secara terbuka karena ide terbaik datang dari mana saja. Jika Anda membaca ini, Anda adalah bagian darinya. Selamat datang. 🦀❤️

## Berkontribusi

Baru di QuantClaw? Cari isu berlabel [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — lihat [Panduan Kontribusi](CONTRIBUTING.md#first-time-contributors) untuk cara memulai. PR yang dibuat dengan AI/vibe-coded dipersilakan! 🤖

Lihat [CONTRIBUTING.md](CONTRIBUTING.md) dan [CLA.md](docs/contributing/cla.md). Implementasikan trait, kirimkan PR:

- Panduan alur kerja CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- `Provider` baru → `src/providers/`
- `Channel` baru → `src/channels/`
- `Observer` baru → `src/observability/`
- `Tool` baru → `src/tools/`
- `Memory` baru → `src/memory/`
- `Tunnel` baru → `src/tunnel/`
- `Peripheral` baru → `src/peripherals/`
- `Skill` baru → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ Repositori Resmi & Peringatan Peniruan

**Ini adalah satu-satunya repositori resmi QuantClaw:**

> https://github.com/quant-speed/quantclaw

Repositori, organisasi, domain, atau paket lain yang mengklaim sebagai "QuantClaw" atau menyiratkan afiliasi dengan QuantClaw Labs adalah **tidak sah dan tidak berafiliasi dengan proyek ini**. Fork tidak sah yang diketahui akan terdaftar di [TRADEMARK.md](docs/maintainers/trademark.md).

Jika Anda menemukan peniruan atau penyalahgunaan merek dagang, silakan [buka isu](https://github.com/quant-speed/quantclaw/issues).

---

## Lisensi

QuantClaw memiliki dual-license untuk keterbukaan maksimum dan perlindungan kontributor:

| Lisensi | Kasus penggunaan |
|---|---|
| [MIT](LICENSE-MIT) | Open-source, riset, akademik, penggunaan pribadi |
| [Apache 2.0](LICENSE-APACHE) | Perlindungan paten, institusional, deployment komersial |

Anda dapat memilih salah satu lisensi. **Kontributor secara otomatis memberikan hak di bawah keduanya** — lihat [CLA.md](docs/contributing/cla.md) untuk perjanjian kontributor lengkap.

### Merek Dagang

Nama dan logo **QuantClaw** adalah merek dagang dari QuantClaw Labs. Lisensi ini tidak memberikan izin untuk menggunakannya untuk menyiratkan dukungan atau afiliasi. Lihat [TRADEMARK.md](docs/maintainers/trademark.md) untuk penggunaan yang diizinkan dan dilarang.

### Perlindungan Kontributor

- Anda **mempertahankan hak cipta** atas kontribusi Anda
- **Hibah paten** (Apache 2.0) melindungi Anda dari klaim paten oleh kontributor lain
- Kontribusi Anda **secara permanen diatribusikan** dalam riwayat commit dan [NOTICE](NOTICE)
- Tidak ada hak merek dagang yang dialihkan dengan berkontribusi

---

**QuantClaw** — Nol overhead. Nol kompromi. Deploy di mana saja. Tukar apa saja. 🦀

## Kontributor

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

Daftar ini dihasilkan dari grafik kontributor GitHub dan diperbarui secara otomatis.

## Riwayat Bintang

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
