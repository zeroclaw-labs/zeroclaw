<p align="center">
  <img src="https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/docs/assets/zeroclaw-banner.png" alt="ZeroClaw" width="600" />
</p>

<h1 align="center">🦀 ZeroClaw — व्यक्तिगत AI सहायक</h1>

<p align="center">
  <strong>शून्य ओवरहेड। शून्य समझौता। 100% Rust। 100% अज्ञेयवादी।</strong><br>
  ⚡️ <strong>$10 के हार्डवेयर पर <5MB RAM के साथ चलता है: यह OpenClaw से 99% कम मेमोरी और Mac mini से 98% सस्ता है!</strong>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/ci-run.yml"><img src="https://img.shields.io/github/actions/workflow/status/zeroclaw-labs/zeroclaw/ci-run.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/releases/latest"><img src="https://img.shields.io/badge/version-v0.6.5-blue" alt="Version v0.6.5" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-Join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Harvard, MIT, और Sundai.Club समुदायों के छात्रों और सदस्यों द्वारा निर्मित।
</p>

<p align="center">
  🌐 <strong>भाषाएँ:</strong>
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

ZeroClaw एक व्यक्तिगत AI सहायक है जिसे आप अपने उपकरणों पर चलाते हैं। यह आपको उन चैनलों पर जवाब देता है जो आप पहले से उपयोग करते हैं (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, और अन्य)। इसमें रियल-टाइम नियंत्रण के लिए एक वेब डैशबोर्ड है और यह हार्डवेयर पेरीफेरल (ESP32, STM32, Arduino, Raspberry Pi) से जुड़ सकता है। Gateway केवल कंट्रोल प्लेन है — उत्पाद सहायक है।

यदि आप एक व्यक्तिगत, एकल-उपयोगकर्ता सहायक चाहते हैं जो स्थानीय, तेज़ और हमेशा चालू महसूस हो, तो यह है।

<p align="center">
  <a href="https://zeroclawlabs.ai">वेबसाइट</a> ·
  <a href="docs/README.md">दस्तावेज़</a> ·
  <a href="docs/architecture.md">आर्किटेक्चर</a> ·
  <a href="#त्वरित-शुरुआत">शुरू करें</a> ·
  <a href="#openclaw-से-माइग्रेशन">OpenClaw से माइग्रेशन</a> ·
  <a href="docs/ops/troubleshooting.md">समस्या निवारण</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **पसंदीदा सेटअप:** अपने टर्मिनल में `zeroclaw onboard` चलाएँ। ZeroClaw Onboard आपको gateway, workspace, channels, और provider सेट करने में कदम-दर-कदम मार्गदर्शन करता है। यह अनुशंसित सेटअप पथ है और macOS, Linux, और Windows (WSL2 के माध्यम से) पर काम करता है। नया इंस्टॉल? यहाँ से शुरू करें: [शुरू करें](#त्वरित-शुरुआत)

### सब्सक्रिप्शन ऑथ (OAuth)

- **OpenAI Codex** (ChatGPT सब्सक्रिप्शन)
- **Gemini** (Google OAuth)
- **Anthropic** (API key या auth token)

मॉडल नोट: जबकि कई प्रदाताओं/मॉडलों का समर्थन किया जाता है, सर्वोत्तम अनुभव के लिए अपने पास उपलब्ध सबसे मजबूत नवीनतम पीढ़ी के मॉडल का उपयोग करें। देखें [ऑनबोर्डिंग](#त्वरित-शुरुआत)।

मॉडल कॉन्फ़िग + CLI: [प्रदाता संदर्भ](docs/reference/api/providers-reference.md)
ऑथ प्रोफ़ाइल रोटेशन (OAuth बनाम API keys) + फ़ेलओवर: [मॉडल फ़ेलओवर](docs/reference/api/providers-reference.md)

## इंस्टॉल (अनुशंसित)

रनटाइम: Rust स्थिर टूलचेन। एकल बाइनरी, कोई रनटाइम निर्भरता नहीं।

### Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### एक-क्लिक बूटस्ट्रैप

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

`zeroclaw onboard` इंस्टॉल के बाद स्वचालित रूप से चलता है ताकि आपका workspace और provider कॉन्फ़िगर हो सके।

## त्वरित शुरुआत (TL;DR)

पूर्ण शुरुआती गाइड (ऑथ, पेयरिंग, चैनल): [शुरू करें](docs/setup-guides/one-click-bootstrap.md)

```bash
# Install + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Start the gateway (webhook server + web dashboard)
zeroclaw gateway                # default: 127.0.0.1:42617
zeroclaw gateway --port 0       # random port (security hardened)

# Talk to the assistant
zeroclaw agent -m "Hello, ZeroClaw!"

# Interactive mode
zeroclaw agent

# Start full autonomous runtime (gateway + channels + cron + hands)
zeroclaw daemon

# Check status
zeroclaw status

# Run diagnostics
zeroclaw doctor
```

अपग्रेड कर रहे हैं? अपडेट के बाद `zeroclaw doctor` चलाएँ।

### स्रोत से (विकास)

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard
```

> **विकास फ़ॉलबैक (कोई ग्लोबल इंस्टॉल नहीं):** कमांड के आगे `cargo run --release --` लगाएँ (उदाहरण: `cargo run --release -- status`)।

## OpenClaw से माइग्रेशन

ZeroClaw आपके OpenClaw workspace, मेमोरी, और कॉन्फ़िगरेशन आयात कर सकता है:

```bash
# Preview what will be migrated (safe, read-only)
zeroclaw migrate openclaw --dry-run

# Run the migration
zeroclaw migrate openclaw
```

यह आपकी मेमोरी प्रविष्टियों, workspace फ़ाइलों, और कॉन्फ़िगरेशन को `~/.openclaw/` से `~/.zeroclaw/` में माइग्रेट करता है। कॉन्फ़िग स्वचालित रूप से JSON से TOML में परिवर्तित हो जाता है।

## सुरक्षा डिफ़ॉल्ट (DM एक्सेस)

ZeroClaw वास्तविक मैसेजिंग सतहों से जुड़ता है। इनबाउंड DMs को अविश्वसनीय इनपुट के रूप में मानें।

पूर्ण सुरक्षा गाइड: [SECURITY.md](SECURITY.md)

सभी चैनलों पर डिफ़ॉल्ट व्यवहार:

- **DM पेयरिंग** (डिफ़ॉल्ट): अज्ञात प्रेषकों को एक छोटा पेयरिंग कोड मिलता है और बॉट उनका संदेश प्रोसेस नहीं करता।
- इससे स्वीकृति दें: `zeroclaw pairing approve <channel> <code>` (फिर प्रेषक स्थानीय अनुमति सूची में जोड़ा जाता है)।
- सार्वजनिक इनबाउंड DMs के लिए `config.toml` में स्पष्ट ऑप्ट-इन आवश्यक है।
- जोखिमपूर्ण या गलत कॉन्फ़िगर DM नीतियों को सामने लाने के लिए `zeroclaw doctor` चलाएँ।

**स्वायत्तता स्तर:**

| स्तर | व्यवहार |
|-------|----------|
| `ReadOnly` | एजेंट देख सकता है लेकिन कार्य नहीं कर सकता |
| `Supervised` (डिफ़ॉल्ट) | एजेंट मध्यम/उच्च जोखिम संचालन के लिए स्वीकृति के साथ कार्य करता है |
| `Full` | एजेंट नीति सीमाओं के भीतर स्वायत्त रूप से कार्य करता है |

**सैंडबॉक्सिंग परतें:** workspace आइसोलेशन, पथ ट्रैवर्सल ब्लॉकिंग, कमांड अनुमति सूची, प्रतिबंधित पथ (`/etc`, `/root`, `~/.ssh`), दर सीमित करना (अधिकतम कार्य/घंटा, लागत/दिन सीमा)।

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 घोषणाएँ

महत्वपूर्ण सूचनाओं (ब्रेकिंग बदलाव, सुरक्षा सलाह, रखरखाव विंडो, और रिलीज़ ब्लॉकर) के लिए इस बोर्ड का उपयोग करें।

| तिथि (UTC) | स्तर | सूचना | कार्रवाई |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _गंभीर_ | हम `openagen/zeroclaw`, `zeroclaw.org` या `zeroclaw.net` से **संबद्ध नहीं** हैं। `zeroclaw.org` और `zeroclaw.net` डोमेन वर्तमान में `openagen/zeroclaw` फ़ोर्क की ओर इशारा करते हैं, और वह डोमेन/रिपॉजिटरी हमारी आधिकारिक वेबसाइट/प्रोजेक्ट का रूप धारण कर रहे हैं। | उन स्रोतों से जानकारी, बाइनरी, फंडरेजिंग, या घोषणाओं पर भरोसा न करें। केवल [यह रिपॉजिटरी](https://github.com/zeroclaw-labs/zeroclaw) और हमारे सत्यापित सोशल अकाउंट्स का उपयोग करें। |
| 2026-02-19 | _महत्वपूर्ण_ | Anthropic ने 2026-02-19 को Authentication and Credential Use शर्तें अपडेट कीं। Claude Code OAuth टोकन (Free, Pro, Max) विशेष रूप से Claude Code और Claude.ai के लिए हैं; Claude Free/Pro/Max से OAuth टोकन का किसी अन्य उत्पाद, उपकरण, या सेवा (Agent SDK सहित) में उपयोग अनुमत नहीं है और उपभोक्ता सेवा की शर्तों का उल्लंघन हो सकता है। | संभावित नुकसान को रोकने के लिए कृपया Claude Code OAuth एकीकरण से अस्थायी रूप से बचें। मूल खंड: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)। |

## मुख्य विशेषताएँ

- **डिफ़ॉल्ट रूप से हल्का रनटाइम** — सामान्य CLI और स्थिति वर्कफ़्लो रिलीज़ बिल्ड पर कुछ-मेगाबाइट मेमोरी एन्वेलप में चलते हैं।
- **लागत-कुशल डिप्लॉयमेंट** — $10 बोर्ड और छोटे क्लाउड इंस्टेंस के लिए डिज़ाइन किया गया, कोई भारी रनटाइम निर्भरता नहीं।
- **तेज़ कोल्ड स्टार्ट** — एकल-बाइनरी Rust रनटाइम कमांड और डेमन स्टार्टअप को लगभग तत्काल रखता है।
- **पोर्टेबल आर्किटेक्चर** — ARM, x86, और RISC-V पर एक बाइनरी जिसमें स्वैपेबल प्रदाता/चैनल/उपकरण हैं।
- **लोकल-फर्स्ट Gateway** — सेशन, चैनल, टूल, cron, SOPs, और इवेंट के लिए एकल कंट्रोल प्लेन।
- **मल्टी-चैनल इनबॉक्स** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket, और अन्य।
- **मल्टी-एजेंट ऑर्केस्ट्रेशन (Hands)** — स्वायत्त एजेंट स्वार्म जो शेड्यूल पर चलते हैं और समय के साथ स्मार्ट होते जाते हैं।
- **मानक संचालन प्रक्रियाएँ (SOPs)** — MQTT, webhook, cron, और पेरीफेरल ट्रिगर के साथ इवेंट-ड्रिवन वर्कफ़्लो ऑटोमेशन।
- **वेब डैशबोर्ड** — React 19 + Vite वेब UI जिसमें रियल-टाइम चैट, मेमोरी ब्राउज़र, कॉन्फ़िग एडिटर, cron मैनेजर, और टूल इंस्पेक्टर है।
- **हार्डवेयर पेरीफेरल** — `Peripheral` trait के माध्यम से ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO।
- **प्रथम-श्रेणी उपकरण** — shell, फ़ाइल I/O, browser, git, वेब fetch/search, MCP, Jira, Notion, Google Workspace, और 70+ अन्य।
- **लाइफसाइकल हुक** — हर चरण पर LLM कॉल, टूल निष्पादन, और संदेशों को इंटरसेप्ट और संशोधित करें।
- **स्किल प्लेटफ़ॉर्म** — बंडल, समुदाय, और workspace स्किल जिनमें सुरक्षा ऑडिटिंग है।
- **टनल सपोर्ट** — रिमोट एक्सेस के लिए Cloudflare, Tailscale, ngrok, OpenVPN, और कस्टम टनल।

### टीमें ZeroClaw क्यों चुनती हैं

- **डिफ़ॉल्ट रूप से हल्का:** छोटी Rust बाइनरी, तेज़ स्टार्टअप, कम मेमोरी फुटप्रिंट।
- **डिज़ाइन से सुरक्षित:** पेयरिंग, सख्त सैंडबॉक्सिंग, स्पष्ट अनुमति सूचियाँ, workspace स्कोपिंग।
- **पूरी तरह से स्वैपेबल:** कोर सिस्टम traits हैं (providers, channels, tools, memory, tunnels)।
- **कोई लॉक-इन नहीं:** OpenAI-संगत प्रदाता समर्थन + प्लगेबल कस्टम एंडपॉइंट।

## बेंचमार्क स्नैपशॉट (ZeroClaw बनाम OpenClaw, प्रतिलिपि योग्य)

स्थानीय मशीन त्वरित बेंचमार्क (macOS arm64, फ़रवरी 2026) 0.8GHz एज हार्डवेयर के लिए सामान्यीकृत।

|                           | OpenClaw      | NanoBot        | PicoClaw        | ZeroClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **भाषा**              | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **स्टार्टअप (0.8GHz कोर)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **बाइनरी आकार**           | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **लागत**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **कोई भी हार्डवेयर $10** |

> नोट: ZeroClaw परिणाम `/usr/bin/time -l` का उपयोग करके रिलीज़ बिल्ड पर मापे गए हैं। OpenClaw को Node.js रनटाइम की आवश्यकता है (आमतौर पर ~390MB अतिरिक्त मेमोरी ओवरहेड), जबकि NanoBot को Python रनटाइम की आवश्यकता है। PicoClaw और ZeroClaw स्टैटिक बाइनरी हैं। ऊपर दिए गए RAM आँकड़े रनटाइम मेमोरी हैं; बिल्ड-टाइम कंपाइलेशन आवश्यकताएँ अधिक हैं।

<p align="center">
  <img src="docs/assets/zeroclaw-comparison.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

### प्रतिलिपि योग्य स्थानीय माप

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

## अब तक हमने जो कुछ बनाया है

### कोर प्लेटफ़ॉर्म

- Gateway HTTP/WS/SSE कंट्रोल प्लेन जिसमें सेशन, प्रेज़ेंस, कॉन्फ़िग, cron, webhooks, वेब डैशबोर्ड, और पेयरिंग है।
- CLI सरफेस: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`।
- एजेंट ऑर्केस्ट्रेशन लूप जिसमें टूल डिस्पैच, प्रॉम्प्ट निर्माण, संदेश वर्गीकरण, और मेमोरी लोडिंग है।
- सुरक्षा नीति प्रवर्तन, स्वायत्तता स्तर, और अनुमोदन गेटिंग के साथ सेशन मॉडल।
- 20+ LLM बैकएंड पर फ़ेलओवर, रिट्राई, और मॉडल रूटिंग के साथ रेज़िलिएंट प्रदाता रैपर।

### चैनल

चैनल: WhatsApp (नेटिव), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk।

फ़ीचर-गेटेड: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`)।

### वेब डैशबोर्ड

React 19 + Vite 6 + Tailwind CSS 4 वेब डैशबोर्ड सीधे Gateway से सर्व किया जाता है:

- **डैशबोर्ड** — सिस्टम अवलोकन, स्वास्थ्य स्थिति, अपटाइम, लागत ट्रैकिंग
- **एजेंट चैट** — एजेंट के साथ इंटरैक्टिव चैट
- **मेमोरी** — मेमोरी प्रविष्टियाँ ब्राउज़ और प्रबंधित करें
- **कॉन्फ़िग** — कॉन्फ़िगरेशन देखें और संपादित करें
- **Cron** — शेड्यूल किए गए कार्य प्रबंधित करें
- **टूल्स** — उपलब्ध उपकरण ब्राउज़ करें
- **लॉग्स** — एजेंट गतिविधि लॉग देखें
- **लागत** — टोकन उपयोग और लागत ट्रैकिंग
- **डॉक्टर** — सिस्टम स्वास्थ्य डायग्नोस्टिक्स
- **इंटीग्रेशन** — इंटीग्रेशन स्थिति और सेटअप
- **पेयरिंग** — डिवाइस पेयरिंग प्रबंधन

### फ़र्मवेयर लक्ष्य

| लक्ष्य | प्लेटफ़ॉर्म | उद्देश्य |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | वायरलेस पेरीफेरल एजेंट |
| ESP32-UI | ESP32 + Display | विज़ुअल इंटरफ़ेस वाला एजेंट |
| STM32 Nucleo | STM32 (ARM Cortex-M) | औद्योगिक पेरीफेरल |
| Arduino | Arduino | बेसिक सेंसर/एक्चुएटर ब्रिज |
| Uno Q Bridge | Arduino Uno | एजेंट के लिए सीरियल ब्रिज |

### उपकरण + ऑटोमेशन

- **कोर:** shell, फ़ाइल read/write/edit, git ऑपरेशन, glob search, content search
- **वेब:** ब्राउज़र नियंत्रण, web fetch, web search, screenshot, image info, PDF read
- **इंटीग्रेशन:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol टूल रैपर + डिफ़र्ड टूल सेट
- **शेड्यूलिंग:** cron add/remove/update/run, schedule tool
- **मेमोरी:** recall, store, forget, knowledge, project intel
- **उन्नत:** delegate (एजेंट-टू-एजेंट), swarm, model switch/routing, security ops, cloud ops
- **हार्डवेयर:** board info, memory map, memory read (फ़ीचर-गेटेड)

### रनटाइम + सुरक्षा

- **स्वायत्तता स्तर:** ReadOnly, Supervised (डिफ़ॉल्ट), Full।
- **सैंडबॉक्सिंग:** workspace आइसोलेशन, पथ ट्रैवर्सल ब्लॉकिंग, कमांड अनुमति सूचियाँ, प्रतिबंधित पथ, Landlock (Linux), Bubblewrap।
- **दर सीमित:** प्रति घंटे अधिकतम कार्य, प्रति दिन अधिकतम लागत (कॉन्फ़िगर योग्य)।
- **अनुमोदन गेटिंग:** मध्यम/उच्च जोखिम संचालन के लिए इंटरैक्टिव अनुमोदन।
- **आपातकालीन रोक:** आपातकालीन शटडाउन क्षमता।
- **129+ सुरक्षा परीक्षण** स्वचालित CI में।

### ऑप्स + पैकेजिंग

- वेब डैशबोर्ड सीधे Gateway से सर्व किया जाता है।
- टनल सपोर्ट: Cloudflare, Tailscale, ngrok, OpenVPN, कस्टम कमांड।
- कंटेनराइज़्ड निष्पादन के लिए Docker रनटाइम एडेप्टर।
- CI/CD: बीटा (पुश पर ऑटो) → स्टेबल (मैनुअल डिस्पैच) → Docker, crates.io, Scoop, AUR, Homebrew, ट्वीट।
- Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64) के लिए प्री-बिल्ट बाइनरी।


## कॉन्फ़िगरेशन

न्यूनतम `~/.zeroclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

पूर्ण कॉन्फ़िगरेशन संदर्भ: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)।

### चैनल कॉन्फ़िगरेशन

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

### टनल कॉन्फ़िगरेशन

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

विवरण: [चैनल संदर्भ](docs/reference/api/channels-reference.md) · [कॉन्फ़िग संदर्भ](docs/reference/api/config-reference.md)

### रनटाइम सपोर्ट (वर्तमान)

- **`native`** (डिफ़ॉल्ट) — सीधा प्रोसेस निष्पादन, सबसे तेज़ पथ, विश्वसनीय वातावरण के लिए आदर्श।
- **`docker`** — पूर्ण कंटेनर आइसोलेशन, लागू सुरक्षा नीतियाँ, Docker आवश्यक।

सख्त सैंडबॉक्सिंग या नेटवर्क आइसोलेशन के लिए `runtime.kind = "docker"` सेट करें।

## सब्सक्रिप्शन ऑथ (OpenAI Codex / Claude Code / Gemini)

ZeroClaw सब्सक्रिप्शन-नेटिव ऑथ प्रोफ़ाइल का समर्थन करता है (मल्टी-अकाउंट, रेस्ट पर एन्क्रिप्टेड)।

- स्टोर फ़ाइल: `~/.zeroclaw/auth-profiles.json`
- एन्क्रिप्शन कुंजी: `~/.zeroclaw/.secret_key`
- प्रोफ़ाइल id फ़ॉर्मेट: `<provider>:<profile_name>` (उदाहरण: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT subscription)
zeroclaw auth login --provider openai-codex --device-code

# Gemini OAuth
zeroclaw auth login --provider gemini --profile default

# Anthropic setup-token
zeroclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Check / refresh / switch profile
zeroclaw auth status
zeroclaw auth refresh --provider openai-codex --profile default
zeroclaw auth use --provider openai-codex --profile work

# Run the agent with subscription auth
zeroclaw agent --provider openai-codex -m "hello"
zeroclaw agent --provider anthropic -m "hello"
```

## एजेंट workspace + स्किल

Workspace रूट: `~/.zeroclaw/workspace/` (कॉन्फ़िग के माध्यम से कॉन्फ़िगर करने योग्य)।

इंजेक्ट किए गए प्रॉम्प्ट फ़ाइलें:
- `IDENTITY.md` — एजेंट का व्यक्तित्व और भूमिका
- `USER.md` — उपयोगकर्ता संदर्भ और प्राथमिकताएँ
- `MEMORY.md` — दीर्घकालिक तथ्य और सबक
- `AGENTS.md` — सेशन सम्मेलन और इनिशियलाइज़ेशन नियम
- `SOUL.md` — कोर पहचान और संचालन सिद्धांत

स्किल: `~/.zeroclaw/workspace/skills/<skill>/SKILL.md` या `SKILL.toml`।

```bash
# List installed skills
zeroclaw skills list

# Install from git
zeroclaw skills install https://github.com/user/my-skill.git

# Security audit before install
zeroclaw skills audit https://github.com/user/my-skill.git

# Remove a skill
zeroclaw skills remove my-skill
```

## CLI कमांड

```bash
# Workspace management
zeroclaw onboard              # Guided setup wizard
zeroclaw status               # Show daemon/agent status
zeroclaw doctor               # Run system diagnostics

# Gateway + daemon
zeroclaw gateway              # Start gateway server (127.0.0.1:42617)
zeroclaw daemon               # Start full autonomous runtime

# Agent
zeroclaw agent                # Interactive chat mode
zeroclaw agent -m "message"   # Single message mode

# Service management
zeroclaw service install      # Install as OS service (launchd/systemd)
zeroclaw service start|stop|restart|status

# Channels
zeroclaw channel list         # List configured channels
zeroclaw channel doctor       # Check channel health
zeroclaw channel bind-telegram 123456789

# Cron + scheduling
zeroclaw cron list            # List scheduled jobs
zeroclaw cron add "*/5 * * * *" --prompt "Check system health"
zeroclaw cron remove <id>

# Memory
zeroclaw memory list          # List memory entries
zeroclaw memory get <key>     # Retrieve a memory
zeroclaw memory stats         # Memory statistics

# Auth profiles
zeroclaw auth login --provider <name>
zeroclaw auth status
zeroclaw auth use --provider <name> --profile <profile>

# Hardware peripherals
zeroclaw hardware discover    # Scan for connected devices
zeroclaw peripheral list      # List connected peripherals
zeroclaw peripheral flash     # Flash firmware to device

# Migration
zeroclaw migrate openclaw --dry-run
zeroclaw migrate openclaw

# Shell completions
source <(zeroclaw completions bash)
zeroclaw completions zsh > ~/.zfunc/_zeroclaw
```

पूर्ण कमांड संदर्भ: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## पूर्वापेक्षाएँ

<details>
<summary><strong>Windows</strong></summary>

#### आवश्यक

1. **Visual Studio Build Tools** (MSVC लिंकर और Windows SDK प्रदान करता है):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    इंस्टॉलेशन के दौरान (या Visual Studio Installer के माध्यम से), **"Desktop development with C++"** वर्कलोड चुनें।

2. **Rust टूलचेन:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    इंस्टॉलेशन के बाद, एक नया टर्मिनल खोलें और `rustup default stable` चलाएँ ताकि स्थिर टूलचेन सक्रिय हो।

3. **सत्यापित करें** कि दोनों काम कर रहे हैं:
    ```powershell
    rustc --version
    cargo --version
    ```

#### वैकल्पिक

- **Docker Desktop** — केवल तभी आवश्यक जब [Docker सैंडबॉक्स्ड रनटाइम](#रनटाइम-सपोर्ट-वर्तमान) (`runtime.kind = "docker"`) का उपयोग कर रहे हों। `winget install Docker.DockerDesktop` से इंस्टॉल करें।

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### आवश्यक

1. **बिल्ड एसेंशियल:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Xcode Command Line Tools इंस्टॉल करें: `xcode-select --install`

2. **Rust टूलचेन:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    विवरण के लिए [rustup.rs](https://rustup.rs) देखें।

3. **सत्यापित करें** कि दोनों काम कर रहे हैं:
    ```bash
    rustc --version
    cargo --version
    ```

#### एक-पंक्ति इंस्टॉलर

या ऊपर के चरणों को छोड़ें और एक ही कमांड में सब कुछ (सिस्टम deps, Rust, ZeroClaw) इंस्टॉल करें:

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

#### कंपाइलेशन संसाधन आवश्यकताएँ

स्रोत से बिल्ड करने के लिए परिणामी बाइनरी चलाने से अधिक संसाधनों की आवश्यकता होती है:

| संसाधन | न्यूनतम | अनुशंसित |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **खाली डिस्क** | 6 GB    | 10 GB+      |

यदि आपका होस्ट न्यूनतम से नीचे है, तो प्री-बिल्ट बाइनरी का उपयोग करें:

```bash
./install.sh --prefer-prebuilt
```

बिना सोर्स फ़ॉलबैक के केवल बाइनरी इंस्टॉल की आवश्यकता के लिए:

```bash
./install.sh --prebuilt-only
```

#### वैकल्पिक

- **Docker** — केवल तभी आवश्यक जब [Docker सैंडबॉक्स्ड रनटाइम](#रनटाइम-सपोर्ट-वर्तमान) (`runtime.kind = "docker"`) का उपयोग कर रहे हों। अपने पैकेज मैनेजर या [docker.com](https://docs.docker.com/engine/install/) से इंस्टॉल करें।

> **नोट:** डिफ़ॉल्ट `cargo build --release` पीक कंपाइल प्रेशर कम करने के लिए `codegen-units=1` का उपयोग करता है। शक्तिशाली मशीनों पर तेज़ बिल्ड के लिए, `cargo build --profile release-fast` का उपयोग करें।

</details>

<!-- markdownlint-enable MD001 MD024 -->

### प्री-बिल्ट बाइनरी

रिलीज़ एसेट इसके लिए प्रकाशित किए जाते हैं:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

नवीनतम एसेट यहाँ से डाउनलोड करें:
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>

## दस्तावेज़

इनका उपयोग तब करें जब आप ऑनबोर्डिंग प्रवाह से आगे हों और गहरा संदर्भ चाहें।

- नेविगेशन और "क्या कहाँ है" के लिए [दस्तावेज़ सूचकांक](docs/README.md) से शुरू करें।
- पूर्ण सिस्टम मॉडल के लिए [आर्किटेक्चर अवलोकन](docs/architecture.md) पढ़ें।
- जब आपको हर कुंजी और उदाहरण चाहिए तो [कॉन्फ़िगरेशन संदर्भ](docs/reference/api/config-reference.md) का उपयोग करें।
- [संचालन रनबुक](docs/ops/operations-runbook.md) के अनुसार Gateway चलाएँ।
- मार्गदर्शित सेटअप के लिए [ZeroClaw Onboard](#त्वरित-शुरुआत) का पालन करें।
- [समस्या निवारण गाइड](docs/ops/troubleshooting.md) से सामान्य विफलताओं का निदान करें।
- कुछ भी एक्सपोज़ करने से पहले [सुरक्षा मार्गदर्शन](docs/security/README.md) की समीक्षा करें।

### संदर्भ दस्तावेज़

- दस्तावेज़ हब: [docs/README.md](docs/README.md)
- एकीकृत दस्तावेज़ TOC: [docs/SUMMARY.md](docs/SUMMARY.md)
- कमांड संदर्भ: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- कॉन्फ़िग संदर्भ: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- प्रदाता संदर्भ: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- चैनल संदर्भ: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- संचालन रनबुक: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- समस्या निवारण: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### सहयोग दस्तावेज़

- योगदान गाइड: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR वर्कफ़्लो नीति: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI वर्कफ़्लो गाइड: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- समीक्षक प्लेबुक: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- सुरक्षा प्रकटीकरण नीति: [SECURITY.md](SECURITY.md)
- दस्तावेज़ टेम्पलेट: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### डिप्लॉयमेंट + संचालन

- नेटवर्क डिप्लॉयमेंट गाइड: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- प्रॉक्सी एजेंट प्लेबुक: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- हार्डवेयर गाइड: [docs/hardware/README.md](docs/hardware/README.md)

## Smooth Crab 🦀

ZeroClaw को smooth crab 🦀 के लिए बनाया गया था, एक तेज़ और कुशल AI सहायक। Argenis De La Rosa और समुदाय द्वारा निर्मित।

- [zeroclawlabs.ai](https://zeroclawlabs.ai)
- [@zeroclawlabs](https://x.com/zeroclawlabs)

## ZeroClaw का समर्थन करें

यदि ZeroClaw आपके काम में मदद करता है और आप चल रहे विकास का समर्थन करना चाहते हैं, तो आप यहाँ दान कर सकते हैं:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 विशेष धन्यवाद

उन समुदायों और संस्थानों को हृदय से धन्यवाद जो इस ओपन-सोर्स कार्य को प्रेरित और ईंधन देते हैं:

- **Harvard University** — बौद्धिक जिज्ञासा को बढ़ावा देने और संभावनाओं की सीमाओं को आगे बढ़ाने के लिए।
- **MIT** — खुले ज्ञान, ओपन सोर्स, और इस विश्वास का समर्थन करने के लिए कि तकनीक सभी के लिए सुलभ होनी चाहिए।
- **Sundai Club** — समुदाय, ऊर्जा, और महत्वपूर्ण चीज़ें बनाने के अथक प्रयास के लिए।
- **दुनिया और उससे परे** 🌍✨ — हर योगदानकर्ता, सपने देखने वाले, और बिल्डर के लिए जो ओपन सोर्स को भलाई की शक्ति बना रहे हैं। यह आपके लिए है।

हम खुले में बना रहे हैं क्योंकि सबसे अच्छे विचार हर जगह से आते हैं। यदि आप यह पढ़ रहे हैं, तो आप इसका हिस्सा हैं। स्वागत है। 🦀❤️

## योगदान

ZeroClaw में नए हैं? [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) लेबल वाले मुद्दों की तलाश करें — शुरू करने का तरीका जानने के लिए हमारा [योगदान गाइड](CONTRIBUTING.md#first-time-contributors) देखें। AI/vibe-coded PRs का स्वागत है! 🤖

[CONTRIBUTING.md](CONTRIBUTING.md) और [CLA.md](docs/contributing/cla.md) देखें। एक trait लागू करें, PR सबमिट करें:

- CI वर्कफ़्लो गाइड: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- नया `Provider` → `src/providers/`
- नया `Channel` → `src/channels/`
- नया `Observer` → `src/observability/`
- नया `Tool` → `src/tools/`
- नया `Memory` → `src/memory/`
- नया `Tunnel` → `src/tunnel/`
- नया `Peripheral` → `src/peripherals/`
- नया `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ आधिकारिक रिपॉजिटरी और प्रतिरूपण चेतावनी

**यह एकमात्र आधिकारिक ZeroClaw रिपॉजिटरी है:**

> https://github.com/zeroclaw-labs/zeroclaw

कोई भी अन्य रिपॉजिटरी, संगठन, डोमेन, या पैकेज जो "ZeroClaw" होने का दावा करता है या ZeroClaw Labs से संबद्धता का संकेत देता है, **अनधिकृत है और इस प्रोजेक्ट से संबद्ध नहीं है**। ज्ञात अनधिकृत फ़ोर्क [TRADEMARK.md](docs/maintainers/trademark.md) में सूचीबद्ध किए जाएँगे।

यदि आप प्रतिरूपण या ट्रेडमार्क दुरुपयोग का सामना करते हैं, तो कृपया [एक इश्यू खोलें](https://github.com/zeroclaw-labs/zeroclaw/issues)।

---

## लाइसेंस

ZeroClaw अधिकतम खुलेपन और योगदानकर्ता सुरक्षा के लिए दोहरे-लाइसेंस प्राप्त है:

| लाइसेंस | उपयोग का मामला |
|---|---|
| [MIT](LICENSE-MIT) | ओपन-सोर्स, अनुसंधान, अकादमिक, व्यक्तिगत उपयोग |
| [Apache 2.0](LICENSE-APACHE) | पेटेंट सुरक्षा, संस्थागत, वाणिज्यिक डिप्लॉयमेंट |

आप कोई भी लाइसेंस चुन सकते हैं। **योगदानकर्ता स्वचालित रूप से दोनों के तहत अधिकार प्रदान करते हैं** — पूर्ण योगदानकर्ता समझौते के लिए [CLA.md](docs/contributing/cla.md) देखें।

### ट्रेडमार्क

**ZeroClaw** नाम और लोगो ZeroClaw Labs के ट्रेडमार्क हैं। यह लाइसेंस समर्थन या संबद्धता का संकेत देने के लिए इनका उपयोग करने की अनुमति नहीं देता। अनुमत और निषिद्ध उपयोग के लिए [TRADEMARK.md](docs/maintainers/trademark.md) देखें।

### योगदानकर्ता सुरक्षा

- आप अपने योगदान का **कॉपीराइट बनाए रखते हैं**
- **पेटेंट अनुदान** (Apache 2.0) आपको अन्य योगदानकर्ताओं द्वारा पेटेंट दावों से बचाता है
- आपके योगदान कमिट इतिहास और [NOTICE](NOTICE) में **स्थायी रूप से श्रेयित** हैं
- योगदान करने से कोई ट्रेडमार्क अधिकार स्थानांतरित नहीं होते

---

**ZeroClaw** — शून्य ओवरहेड। शून्य समझौता। कहीं भी डिप्लॉय करें। कुछ भी स्वैप करें। 🦀

## योगदानकर्ता

<a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
</a>

यह सूची GitHub योगदानकर्ता ग्राफ़ से उत्पन्न होती है और स्वचालित रूप से अपडेट होती है।

## स्टार इतिहास

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
