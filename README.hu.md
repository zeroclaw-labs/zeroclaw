<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Nulla többletköltség. Nulla kompromisszum. 100% Rust. 100% Agnosztikus.</strong><br>
  ⚡️ <strong>$10-es hardveren fut <5MB RAM-mal: Ez 99%-kal kevesebb memória, mint az OpenClaw és 98%-kal olcsóbb, mint egy Mac mini!</strong>
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
  🌐 <strong>Nyelvek:</strong>
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

## Mi az a ZeroClaw?

A ZeroClaw egy könnyűsúlyú, változtatható és bővíthető AI asszisztens infrastruktúra, amely Rust nyelven készült. Különböző LLM szolgáltatókat (Anthropic, OpenAI, Google, Ollama stb.) köt össze egy egységes felületen keresztül, és több csatornát támogat (Telegram, Matrix, CLI stb.).

### Fő jellemzők

- **🦀 Rust nyelven írva**: Magas teljesítmény, memória biztonság és null költségű absztrakciók
- **🔌 Szolgáltató-agnosztikus**: OpenAI, Anthropic, Google Gemini, Ollama és mások támogatása
- **📱 Többcsatornás**: Telegram, Matrix (E2EE-vel), CLI és mások
- **🧠 Cserélhető memória**: SQLite és Markdown backendek
- **🛠️ Bővíthető eszközök**: Egyszerűen adjon hozzá egyedi eszközöket
- **🔒 Biztonság először**: Fordított proxy, adatvédelem-elsődleges tervezés

---

## Gyors Kezdés

### Követelmények

- Rust 1.70+
- Egy LLM szolgáltató API kulcs (Anthropic, OpenAI stb.)

### Telepítés

```bash
# Klónozza a repositoryt
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Építés
cargo build --release

# Futtatás
cargo run --release
```

### Docker-rel

```bash
docker run -d \
  --name zeroclaw \
  -e ANTHROPIC_API_KEY=your_key \
  -v zeroclaw-data:/app/data \
  zeroclaw/zeroclaw:latest
```

---

## Konfiguráció

A ZeroClaw egy YAML konfigurációs fájlt használ. Alapértelmezés szerint a `config.yaml` fájlt keresi.

```yaml
# Alapértelmezett szolgáltató
provider: anthropic

# Szolgáltató konfiguráció
providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
    model: claude-3-5-sonnet-20241022
  openai:
    api_key: ${OPENAI_API_KEY}
    model: gpt-4o

# Memória konfiguráció
memory:
  backend: sqlite
  path: data/memory.db

# Csatorna konfiguráció
channels:
  telegram:
    token: ${TELEGRAM_BOT_TOKEN}
```

---

## Dokumentáció

Részletes dokumentációért lásd:

- [Dokumentációs Központ](docs/README.md)
- [Parancs Referencia](docs/commands-reference.md)
- [Szolgáltató Referencia](docs/providers-reference.md)
- [Csatorna Referencia](docs/channels-reference.md)
- [Konfigurációs Referencia](docs/config-reference.md)

---

## Hozzájárulás

A hozzájárulások várják! Kérjük, olvassa el a [Hozzájárulási Útmutatót](CONTRIBUTING.md).

---

## Licenc

Ez a projekt kettős licencelt:

- MIT License
- Apache License, 2.0 verzió

Részletekért lásd a [LICENSE-APACHE](LICENSE-APACHE) és [LICENSE-MIT](LICENSE-MIT) fájlokat.

---

## Közösség

- [Telegram](https://t.me/zeroclawlabs)
- [Facebook Group](https://www.facebook.com/groups/zeroclaw)
- [WeChat Group](https://zeroclawlabs.cn/group.jpg)

---

## Szponzorok

Ha a ZeroClaw hasznos az Ön számára, kérjük, fontolja meg, hogy vesz nekünk egy kávét:

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee)](https://buymeacoffee.com/argenistherose)
