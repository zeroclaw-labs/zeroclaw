<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Nul overhead. Nul kompromis. 100% Rust. 100% Agnostisk.</strong><br>
  ⚡️ <strong>Kører på $10 hardware med <5MB RAM: Det er 99% mindre hukommelse end OpenClaw og 98% billigere end en Mac mini!</strong>
</p>

<p align="center">
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="NOTICE"><img src="https://img.shields.io/badge/contributors-27+-green.svg" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://zeroclawlabs.cn/group.jpg"><img src="https://img.shields.io/badge/WeChat-Group-B7D7A8?logo=wechat&logoColor=white" alt="WeChat Group" /></a>
  <a href="https://www.xiaohongshu.com/user/profile/67cbfc43000000000d008307?xsec_token=AB73VnYnGNx5y36EtnnZfGmAmS-6Wzv8WMuGpfwfkg6Yc%3D&xsec_source=pc_search"><img src="https://img.shields.io/badge/Xiaohongshu-Official-FF2442?style=flat" alt="Xiaohongshu: Official" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.facebook.com/groups/zeroclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Facebook Group" /></a>
</p>

<p align="center">
  🌐 <strong>Sprog:</strong>
  <a href="README.md">🇺🇸 English</a> ·
  <a href="README.zh-CN.md">🇨🇳 简体中文</a> ·
  <a href="README.ja.md">🇯🇵 日本語</a> ·
  <a href="README.ko.md">🇰🇷 한국어</a> ·
  <a href="README.vi.md">🇻🇳 Tiếng Việt</a> ·
  <a href="README.tl.md">🇵🇭 Tagalog</a> ·
  <a href="README.es.md">🇪🇸 Español</a> ·
  <a href="README.pt.md">🇧🇷 Português</a> ·
  <a href="README.it.md">🇮🇹 Italiano</a> ·
  <a href="README.de.md">🇩🇪 Deutsch</a> ·
  <a href="README.fr.md">🇫🇷 Français</a> ·
  <a href="README.ar.md">🇸🇦 العربية</a> ·
  <a href="README.hi.md">🇮🇳 हिन्दी</a> ·
  <a href="README.ru.md">🇷🇺 Русский</a> ·
  <a href="README.bn.md">🇧🇩 বাংলা</a> ·
  <a href="README.he.md">🇮🇱 עברית</a> ·
  <a href="README.pl.md">🇵🇱 Polski</a> ·
  <a href="README.cs.md">🇨🇿 Čeština</a> ·
  <a href="README.nl.md">🇳🇱 Nederlands</a> ·
  <a href="README.tr.md">🇹🇷 Türkçe</a> ·
  <a href="README.uk.md">🇺🇦 Українська</a> ·
  <a href="README.id.md">🇮🇩 Bahasa Indonesia</a> ·
  <a href="README.th.md">🇹🇭 ไทย</a> ·
  <a href="README.ur.md">🇵🇰 اردو</a> ·
  <a href="README.ro.md">🇷🇴 Română</a> ·
  <a href="README.sv.md">🇸🇪 Svenska</a> ·
  <a href="README.el.md">🇬🇷 Ελληνικά</a> ·
  <a href="README.hu.md">🇭🇺 Magyar</a> ·
  <a href="README.fi.md">🇫🇮 Suomi</a> ·
  <a href="README.da.md">🇩🇰 Dansk</a> ·
  <a href="README.nb.md">🇳🇴 Norsk</a>
</p>

---

<!-- BEGIN:WHATS_NEW -->

### 🚀 What's New in v0.3.1-beta.205 (March 2026)

| Area | Highlights |
|---|---|
| ci | add Termux (aarch64-linux-android) release target |

<!-- END:WHATS_NEW -->

## Hvad er ZeroClaw?

ZeroClaw er en letvægts, foranderlig og udvidbar AI-assistent-infrastruktur bygget i Rust. Den forbinder forskellige LLM-udbydere (Anthropic, OpenAI, Google, Ollama osv.) via en samlet grænseflade og understøtter flere kanaler (Telegram, Matrix, CLI osv.).

### Nøglefunktioner

- **🦀 Skrevet i Rust**: Høj ydeevne, hukommelsessikkerhed og nul-omkostningsabstraktioner
- **🔌 Udbyder-agnostisk**: Understøtter OpenAI, Anthropic, Google Gemini, Ollama og andre
- **📱 Multi-kanal**: Telegram, Matrix (med E2EE), CLI og andre
- **🧠 Pluggbar hukommelse**: SQLite og Markdown-backends
- **🛠️ Udvidbare værktøjer**: Tilføj brugerdefinerede værktøjer nemt
- **🔒 Sikkerhed først**: Omvendt proxy, privatlivs-først design

---

## Hurtig Start

### Krav

- Rust 1.70+
- En LLM-udbyder API-nøgle (Anthropic, OpenAI osv.)

### Installation

```bash
# Klon repository
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Byg
cargo build --release

# Kør
cargo run --release
```

### Med Docker

```bash
docker run -d \
  --name zeroclaw \
  -e ANTHROPIC_API_KEY=your_key \
  -v zeroclaw-data:/app/data \
  zeroclaw/zeroclaw:latest
```

---

## Konfiguration

ZeroClaw bruger en YAML-konfigurationsfil. Som standard leder den efter `config.yaml`.

```yaml
# Standardudbyder
provider: anthropic

# Udbyderkonfiguration
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
    model: claude-3-5-sonnet-20241022
  openai:
    api_key: ${OPENAI_API_KEY}
    model: gpt-4o

# Hukommelseskonfiguration
memory:
  backend: sqlite
  path: data/memory.db

# Kanalkonfiguration
channels:
  telegram:
    token: ${TELEGRAM_BOT_TOKEN}
```

---

## Dokumentation

For detaljeret dokumentation, se:

- [Dokumentationshub](docs/README.md)
- [Kommandoreference](docs/commands-reference.md)
- [Udbyderreference](docs/providers-reference.md)
- [Kanalreference](docs/channels-reference.md)
- [Konfigurationsreference](docs/config-reference.md)

---

## Bidrag

Bidrag er velkomne! Læs venligst [Bidragsguiden](CONTRIBUTING.md).

---

## Licens

Dette projekt er dobbelt-licenseret:

- MIT License
- Apache License, version 2.0

Se [LICENSE-APACHE](LICENSE-APACHE) og [LICENSE-MIT](LICENSE-MIT) for detaljer.

---

## Fællesskab

- [Telegram](https://t.me/zeroclawlabs)
- [Facebook Group](https://www.facebook.com/groups/zeroclaw)
- [WeChat Group](https://zeroclawlabs.cn/group.jpg)

---

## Sponsorer

Hvis ZeroClaw er nyttigt for dig, overvej venligst at købe os en kaffe:

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee)](https://buymeacoffee.com/argenistherose)

<!-- BEGIN:RECENT_CONTRIBUTORS -->

### 🌟 Recent Contributors (v0.3.1-beta.205)

3 contributors shipped features, fixes, and improvements in this release cycle:

- **Argenis**
- **argenis de la rosa**
- **Claude Opus 4.6**

Thank you to everyone who opened issues, reviewed PRs, translated docs, and helped test. Every contribution matters. 🦀

<!-- END:RECENT_CONTRIBUTORS -->
