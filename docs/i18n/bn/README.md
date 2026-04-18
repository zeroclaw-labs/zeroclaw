<p align="center">
  <img src="https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/docs/assets/zeroclaw-banner.png" alt="ZeroClaw" width="600" />
</p>

<h1 align="center">🦀 ZeroClaw — ব্যক্তিগত AI সহকারী</h1>

<p align="center">
  <strong>শূন্য ওভারহেড। শূন্য আপস। 100% Rust। 100% অজ্ঞেয়বাদী।</strong><br>
  ⚡️ <strong>$10 হার্ডওয়্যারে <5MB RAM দিয়ে চলে: এটি OpenClaw থেকে 99% কম মেমোরি এবং Mac mini থেকে 98% সস্তা!</strong>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/ci-run.yml"><img src="https://img.shields.io/github/actions/workflow/status/zeroclaw-labs/zeroclaw/ci-run.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/releases/latest"><img src="https://img.shields.io/badge/version-v0.7.1-blue" alt="Version v0.7.1" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-Join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Harvard, MIT, এবং Sundai.Club সম্প্রদায়ের ছাত্র ও সদস্যদের দ্বারা নির্মিত।
</p>

<p align="center">
  🌐 <strong>ভাষাসমূহ:</strong>
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

ZeroClaw একটি ব্যক্তিগত AI সহকারী যা আপনি আপনার নিজের ডিভাইসে চালান। এটি আপনাকে সেই চ্যানেলগুলোতে উত্তর দেয় যা আপনি ইতিমধ্যে ব্যবহার করেন (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, এবং আরও)। এতে রিয়েল-টাইম নিয়ন্ত্রণের জন্য একটি ওয়েব ড্যাশবোর্ড আছে এবং এটি হার্ডওয়্যার পেরিফেরালের (ESP32, STM32, Arduino, Raspberry Pi) সাথে সংযোগ করতে পারে। Gateway শুধুমাত্র কন্ট্রোল প্লেন — পণ্যটি হল সহকারী।

আপনি যদি একটি ব্যক্তিগত, একক-ব্যবহারকারী সহকারী চান যা স্থানীয়, দ্রুত এবং সর্বদা চালু মনে হয়, এটাই সেটি।

<p align="center">
  <a href="https://zeroclawlabs.ai">ওয়েবসাইট</a> ·
  <a href="docs/README.md">ডকুমেন্টেশন</a> ·
  <a href="docs/architecture.md">আর্কিটেকচার</a> ·
  <a href="#দ্রুত-শুরু">শুরু করুন</a> ·
  <a href="#openclaw-থেকে-মাইগ্রেশন">OpenClaw থেকে মাইগ্রেশন</a> ·
  <a href="docs/ops/troubleshooting.md">সমস্যা সমাধান</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **পছন্দের সেটআপ:** আপনার টার্মিনালে `zeroclaw onboard` চালান। ZeroClaw Onboard আপনাকে gateway, workspace, channels, এবং provider সেট আপ করতে ধাপে ধাপে গাইড করে। এটি প্রস্তাবিত সেটআপ পথ এবং macOS, Linux, এবং Windows (WSL2 এর মাধ্যমে) এ কাজ করে। নতুন ইনস্টল? এখানে শুরু করুন: [শুরু করুন](#দ্রুত-শুরু)

### সাবস্ক্রিপশন অথ (OAuth)

- **OpenAI Codex** (ChatGPT সাবস্ক্রিপশন)
- **Gemini** (Google OAuth)
- **Anthropic** (API key বা auth token)

মডেল নোট: যদিও অনেক প্রদানকারী/মডেল সমর্থিত, সেরা অভিজ্ঞতার জন্য আপনার কাছে উপলব্ধ সবচেয়ে শক্তিশালী সর্বশেষ প্রজন্মের মডেল ব্যবহার করুন। দেখুন [অনবোর্ডিং](#দ্রুত-শুরু)।

মডেল কনফিগ + CLI: [প্রদানকারী রেফারেন্স](docs/reference/api/providers-reference.md)
অথ প্রোফাইল রোটেশন (OAuth বনাম API keys) + ফেইলওভার: [মডেল ফেইলওভার](docs/reference/api/providers-reference.md)

## ইনস্টল (প্রস্তাবিত)

রানটাইম: Rust স্থিতিশীল টুলচেইন। একক বাইনারি, কোনো রানটাইম নির্ভরতা নেই।

### Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### এক-ক্লিক বুটস্ট্র্যাপ

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

`zeroclaw onboard` ইনস্টলের পরে স্বয়ংক্রিয়ভাবে চলে আপনার workspace এবং provider কনফিগার করতে।

## দ্রুত শুরু (TL;DR)

সম্পূর্ণ শিক্ষানবিশ গাইড (অথ, পেয়ারিং, চ্যানেল): [শুরু করুন](docs/setup-guides/one-click-bootstrap.md)

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

আপগ্রেড করছেন? আপডেটের পরে `zeroclaw doctor` চালান।

### সোর্স থেকে (ডেভেলপমেন্ট)

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard
```

> **ডেভ ফলব্যাক (কোনো গ্লোবাল ইনস্টল নেই):** কমান্ডের আগে `cargo run --release --` যোগ করুন (উদাহরণ: `cargo run --release -- status`)।

## OpenClaw থেকে মাইগ্রেশন

ZeroClaw আপনার OpenClaw workspace, মেমোরি, এবং কনফিগারেশন আমদানি করতে পারে:

```bash
# Preview what will be migrated (safe, read-only)
zeroclaw migrate openclaw --dry-run

# Run the migration
zeroclaw migrate openclaw
```

এটি আপনার মেমোরি এন্ট্রি, workspace ফাইল, এবং কনফিগারেশন `~/.openclaw/` থেকে `~/.zeroclaw/` তে মাইগ্রেট করে। কনফিগ স্বয়ংক্রিয়ভাবে JSON থেকে TOML এ রূপান্তরিত হয়।

## নিরাপত্তা ডিফল্ট (DM অ্যাক্সেস)

ZeroClaw প্রকৃত মেসেজিং সারফেসের সাথে সংযোগ করে। ইনবাউন্ড DM গুলোকে অবিশ্বস্ত ইনপুট হিসেবে বিবেচনা করুন।

সম্পূর্ণ নিরাপত্তা গাইড: [SECURITY.md](SECURITY.md)

সকল চ্যানেলে ডিফল্ট আচরণ:

- **DM পেয়ারিং** (ডিফল্ট): অজানা প্রেরকরা একটি সংক্ষিপ্ত পেয়ারিং কোড পায় এবং বট তাদের বার্তা প্রক্রিয়া করে না।
- এর মাধ্যমে অনুমোদন করুন: `zeroclaw pairing approve <channel> <code>` (তারপর প্রেরক স্থানীয় অনুমতি তালিকায় যুক্ত হয়)।
- পাবলিক ইনবাউন্ড DM এর জন্য `config.toml` এ স্পষ্ট অপ্ট-ইন প্রয়োজন।
- ঝুঁকিপূর্ণ বা ভুল কনফিগার করা DM নীতি প্রকাশ করতে `zeroclaw doctor` চালান।

**স্বায়ত্তশাসন স্তর:**

| স্তর | আচরণ |
|-------|----------|
| `ReadOnly` | এজেন্ট পর্যবেক্ষণ করতে পারে কিন্তু কাজ করতে পারে না |
| `Supervised` (ডিফল্ট) | এজেন্ট মাঝারি/উচ্চ ঝুঁকি অপারেশনের জন্য অনুমোদন সহ কাজ করে |
| `Full` | এজেন্ট নীতি সীমার মধ্যে স্বায়ত্তশাসিতভাবে কাজ করে |

**স্যান্ডবক্সিং স্তর:** workspace আইসোলেশন, পাথ ট্রাভার্সাল ব্লকিং, কমান্ড অনুমতি তালিকা, নিষিদ্ধ পাথ (`/etc`, `/root`, `~/.ssh`), রেট লিমিটিং (সর্বোচ্চ কার্য/ঘণ্টা, খরচ/দিন সীমা)।

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 ঘোষণা

গুরুত্বপূর্ণ নোটিশের (ব্রেকিং পরিবর্তন, নিরাপত্তা পরামর্শ, রক্ষণাবেক্ষণ উইন্ডো, এবং রিলিজ ব্লকার) জন্য এই বোর্ড ব্যবহার করুন।

| তারিখ (UTC) | স্তর | নোটিশ | পদক্ষেপ |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _জটিল_ | আমরা `openagen/zeroclaw`, `zeroclaw.org` বা `zeroclaw.net` এর সাথে **সম্পর্কিত নই**। `zeroclaw.org` এবং `zeroclaw.net` ডোমেইনগুলো বর্তমানে `openagen/zeroclaw` ফর্কের দিকে নির্দেশ করে, এবং সেই ডোমেইন/রিপোজিটরি আমাদের অফিসিয়াল ওয়েবসাইট/প্রকল্পের ছদ্মবেশ ধারণ করছে। | সেই উৎসগুলো থেকে তথ্য, বাইনারি, তহবিল সংগ্রহ, বা ঘোষণায় বিশ্বাস করবেন না। শুধুমাত্র [এই রিপোজিটরি](https://github.com/zeroclaw-labs/zeroclaw) এবং আমাদের যাচাইকৃত সোশ্যাল অ্যাকাউন্ট ব্যবহার করুন। |
| 2026-02-19 | _গুরুত্বপূর্ণ_ | Anthropic 2026-02-19 তে Authentication and Credential Use শর্তাবলী আপডেট করেছে। Claude Code OAuth টোকেন (Free, Pro, Max) একচেটিয়াভাবে Claude Code এবং Claude.ai এর জন্য; Claude Free/Pro/Max থেকে OAuth টোকেন অন্য কোনো পণ্য, টুল, বা সেবায় (Agent SDK সহ) ব্যবহার অনুমোদিত নয় এবং Consumer Terms of Service লঙ্ঘন করতে পারে। | সম্ভাব্য ক্ষতি রোধ করতে অনুগ্রহ করে Claude Code OAuth ইন্টিগ্রেশন সাময়িকভাবে এড়িয়ে চলুন। মূল ধারা: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)। |

## প্রধান বৈশিষ্ট্য

- **ডিফল্টভাবে হালকা রানটাইম** — সাধারণ CLI এবং স্ট্যাটাস ওয়ার্কফ্লো রিলিজ বিল্ডে কয়েক-মেগাবাইট মেমোরি এনভেলপে চলে।
- **খরচ-সাশ্রয়ী ডিপ্লয়মেন্ট** — $10 বোর্ড এবং ছোট ক্লাউড ইনস্ট্যান্সের জন্য ডিজাইন করা, কোনো ভারী রানটাইম নির্ভরতা নেই।
- **দ্রুত কোল্ড স্টার্ট** — একক-বাইনারি Rust রানটাইম কমান্ড এবং ডেমন স্টার্টআপ প্রায় তাৎক্ষণিক রাখে।
- **পোর্টেবল আর্কিটেকচার** — ARM, x86, এবং RISC-V জুড়ে একটি বাইনারি যার সাথে বিনিময়যোগ্য প্রদানকারী/চ্যানেল/টুল।
- **লোকাল-ফার্স্ট Gateway** — সেশন, চ্যানেল, টুল, cron, SOPs, এবং ইভেন্টের জন্য একক কন্ট্রোল প্লেন।
- **মাল্টি-চ্যানেল ইনবক্স** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket, এবং আরও।
- **মাল্টি-এজেন্ট অর্কেস্ট্রেশন (Hands)** — স্বায়ত্তশাসিত এজেন্ট সোয়ার্ম যা সময়সূচী অনুযায়ী চলে এবং সময়ের সাথে আরও স্মার্ট হয়।
- **স্ট্যান্ডার্ড অপারেটিং প্রসিডিউর (SOPs)** — MQTT, webhook, cron, এবং পেরিফেরাল ট্রিগার সহ ইভেন্ট-চালিত ওয়ার্কফ্লো অটোমেশন।
- **ওয়েব ড্যাশবোর্ড** — React 19 + Vite ওয়েব UI যাতে রিয়েল-টাইম চ্যাট, মেমোরি ব্রাউজার, কনফিগ এডিটর, cron ম্যানেজার, এবং টুল ইন্সপেক্টর আছে।
- **হার্ডওয়্যার পেরিফেরাল** — `Peripheral` trait এর মাধ্যমে ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO।
- **প্রথম-শ্রেণীর টুল** — shell, ফাইল I/O, browser, git, ওয়েব fetch/search, MCP, Jira, Notion, Google Workspace, এবং 70+ আরও।
- **লাইফসাইকেল হুক** — প্রতিটি পর্যায়ে LLM কল, টুল এক্সিকিউশন, এবং বার্তা ইন্টারসেপ্ট ও পরিবর্তন করুন।
- **স্কিল প্ল্যাটফর্ম** — নিরাপত্তা অডিটিং সহ বান্ডেল, সম্প্রদায়, এবং workspace স্কিল।
- **টানেল সাপোর্ট** — রিমোট অ্যাক্সেসের জন্য Cloudflare, Tailscale, ngrok, OpenVPN, এবং কাস্টম টানেল।

### দলগুলো কেন ZeroClaw বেছে নেয়

- **ডিফল্টভাবে হালকা:** ছোট Rust বাইনারি, দ্রুত স্টার্টআপ, কম মেমোরি ফুটপ্রিন্ট।
- **ডিজাইনে নিরাপদ:** পেয়ারিং, কঠোর স্যান্ডবক্সিং, স্পষ্ট অনুমতি তালিকা, workspace স্কোপিং।
- **সম্পূর্ণ বিনিময়যোগ্য:** মূল সিস্টেমগুলো traits (providers, channels, tools, memory, tunnels)।
- **কোনো লক-ইন নেই:** OpenAI-সামঞ্জস্যপূর্ণ প্রদানকারী সমর্থন + প্লাগেবল কাস্টম এন্ডপয়েন্ট।

## বেঞ্চমার্ক স্ন্যাপশট (ZeroClaw বনাম OpenClaw, পুনরুৎপাদনযোগ্য)

স্থানীয় মেশিন দ্রুত বেঞ্চমার্ক (macOS arm64, ফেব্রুয়ারি 2026) 0.8GHz এজ হার্ডওয়্যারের জন্য স্বাভাবিকীকৃত।

|                           | OpenClaw      | NanoBot        | PicoClaw        | ZeroClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **ভাষা**              | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **স্টার্টআপ (0.8GHz কোর)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **বাইনারি আকার**           | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **খরচ**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **যেকোনো হার্ডওয়্যার $10** |

> নোট: ZeroClaw ফলাফল `/usr/bin/time -l` ব্যবহার করে রিলিজ বিল্ডে পরিমাপ করা হয়েছে। OpenClaw এর Node.js রানটাইম প্রয়োজন (সাধারণত ~390MB অতিরিক্ত মেমোরি ওভারহেড), যেখানে NanoBot এর Python রানটাইম প্রয়োজন। PicoClaw এবং ZeroClaw স্ট্যাটিক বাইনারি। উপরের RAM পরিসংখ্যান রানটাইম মেমোরি; বিল্ড-টাইম কম্পাইলেশন প্রয়োজনীয়তা বেশি।

<p align="center">
  <img src="docs/assets/zeroclaw-comparison.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

### পুনরুৎপাদনযোগ্য স্থানীয় পরিমাপ

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

## এখন পর্যন্ত আমরা যা তৈরি করেছি

### কোর প্ল্যাটফর্ম

- Gateway HTTP/WS/SSE কন্ট্রোল প্লেন যাতে সেশন, উপস্থিতি, কনফিগ, cron, webhooks, ওয়েব ড্যাশবোর্ড, এবং পেয়ারিং আছে।
- CLI সারফেস: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`।
- এজেন্ট অর্কেস্ট্রেশন লুপ যাতে টুল ডিসপ্যাচ, প্রম্পট নির্মাণ, বার্তা শ্রেণীবিভাগ, এবং মেমোরি লোডিং আছে।
- নিরাপত্তা নীতি প্রয়োগ, স্বায়ত্তশাসন স্তর, এবং অনুমোদন গেটিং সহ সেশন মডেল।
- 20+ LLM ব্যাকএন্ড জুড়ে ফেইলওভার, রিট্রাই, এবং মডেল রাউটিং সহ রেজিলিয়েন্ট প্রদানকারী র‍্যাপার।

### চ্যানেল

চ্যানেল: WhatsApp (নেটিভ), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk।

ফিচার-গেটেড: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`)।

### ওয়েব ড্যাশবোর্ড

React 19 + Vite 6 + Tailwind CSS 4 ওয়েব ড্যাশবোর্ড সরাসরি Gateway থেকে পরিবেশিত:

- **ড্যাশবোর্ড** — সিস্টেম ওভারভিউ, স্বাস্থ্য অবস্থা, আপটাইম, খরচ ট্র্যাকিং
- **এজেন্ট চ্যাট** — এজেন্টের সাথে ইন্টারেক্টিভ চ্যাট
- **মেমোরি** — মেমোরি এন্ট্রি ব্রাউজ ও পরিচালনা
- **কনফিগ** — কনফিগারেশন দেখুন ও সম্পাদনা করুন
- **Cron** — নির্ধারিত কাজ পরিচালনা
- **টুলস** — উপলব্ধ টুল ব্রাউজ করুন
- **লগস** — এজেন্ট কার্যকলাপ লগ দেখুন
- **খরচ** — টোকেন ব্যবহার এবং খরচ ট্র্যাকিং
- **ডক্টর** — সিস্টেম স্বাস্থ্য ডায়াগনস্টিকস
- **ইন্টিগ্রেশন** — ইন্টিগ্রেশন অবস্থা এবং সেটআপ
- **পেয়ারিং** — ডিভাইস পেয়ারিং পরিচালনা

### ফার্মওয়্যার টার্গেট

| টার্গেট | প্ল্যাটফর্ম | উদ্দেশ্য |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | ওয়্যারলেস পেরিফেরাল এজেন্ট |
| ESP32-UI | ESP32 + Display | ভিজ্যুয়াল ইন্টারফেস সহ এজেন্ট |
| STM32 Nucleo | STM32 (ARM Cortex-M) | ইন্ডাস্ট্রিয়াল পেরিফেরাল |
| Arduino | Arduino | বেসিক সেন্সর/অ্যাকচুয়েটর ব্রিজ |
| Uno Q Bridge | Arduino Uno | এজেন্টের জন্য সিরিয়াল ব্রিজ |

### টুল + অটোমেশন

- **কোর:** shell, ফাইল read/write/edit, git অপারেশন, glob search, content search
- **ওয়েব:** ব্রাউজার নিয়ন্ত্রণ, web fetch, web search, screenshot, image info, PDF read
- **ইন্টিগ্রেশন:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol টুল র‍্যাপার + ডিফার্ড টুল সেট
- **শিডিউলিং:** cron add/remove/update/run, schedule tool
- **মেমোরি:** recall, store, forget, knowledge, project intel
- **উন্নত:** delegate (এজেন্ট-টু-এজেন্ট), swarm, model switch/routing, security ops, cloud ops
- **হার্ডওয়্যার:** board info, memory map, memory read (ফিচার-গেটেড)

### রানটাইম + নিরাপত্তা

- **স্বায়ত্তশাসন স্তর:** ReadOnly, Supervised (ডিফল্ট), Full।
- **স্যান্ডবক্সিং:** workspace আইসোলেশন, পাথ ট্রাভার্সাল ব্লকিং, কমান্ড অনুমতি তালিকা, নিষিদ্ধ পাথ, Landlock (Linux), Bubblewrap।
- **রেট লিমিটিং:** প্রতি ঘণ্টায় সর্বোচ্চ কার্য, প্রতি দিনে সর্বোচ্চ খরচ (কনফিগারযোগ্য)।
- **অনুমোদন গেটিং:** মাঝারি/উচ্চ ঝুঁকি অপারেশনের জন্য ইন্টারেক্টিভ অনুমোদন।
- **ই-স্টপ:** জরুরি শাটডাউন ক্ষমতা।
- **129+ নিরাপত্তা পরীক্ষা** স্বয়ংক্রিয় CI তে।

### অপস + প্যাকেজিং

- ওয়েব ড্যাশবোর্ড সরাসরি Gateway থেকে পরিবেশিত।
- টানেল সাপোর্ট: Cloudflare, Tailscale, ngrok, OpenVPN, কাস্টম কমান্ড।
- কন্টেইনারাইজড এক্সিকিউশনের জন্য Docker রানটাইম অ্যাডাপ্টার।
- CI/CD: বেটা (পুশে অটো) → স্টেবল (ম্যানুয়াল ডিসপ্যাচ) → Docker, crates.io, Scoop, AUR, Homebrew, টুইট।
- Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64) এর জন্য প্রি-বিল্ট বাইনারি।


## কনফিগারেশন

ন্যূনতম `~/.zeroclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

সম্পূর্ণ কনফিগারেশন রেফারেন্স: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)।

### চ্যানেল কনফিগারেশন

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

### টানেল কনফিগারেশন

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

বিস্তারিত: [চ্যানেল রেফারেন্স](docs/reference/api/channels-reference.md) · [কনফিগ রেফারেন্স](docs/reference/api/config-reference.md)

### রানটাইম সাপোর্ট (বর্তমান)

- **`native`** (ডিফল্ট) — সরাসরি প্রসেস এক্সিকিউশন, দ্রুততম পথ, বিশ্বস্ত পরিবেশের জন্য আদর্শ।
- **`docker`** — সম্পূর্ণ কন্টেইনার আইসোলেশন, প্রয়োগকৃত নিরাপত্তা নীতি, Docker প্রয়োজন।

কঠোর স্যান্ডবক্সিং বা নেটওয়ার্ক আইসোলেশনের জন্য `runtime.kind = "docker"` সেট করুন।

## সাবস্ক্রিপশন অথ (OpenAI Codex / Claude Code / Gemini)

ZeroClaw সাবস্ক্রিপশন-নেটিভ অথ প্রোফাইল সমর্থন করে (মাল্টি-অ্যাকাউন্ট, বিশ্রামে এনক্রিপ্টেড)।

- স্টোর ফাইল: `~/.zeroclaw/auth-profiles.json`
- এনক্রিপশন কী: `~/.zeroclaw/.secret_key`
- প্রোফাইল id ফরম্যাট: `<provider>:<profile_name>` (উদাহরণ: `openai-codex:work`)

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

## এজেন্ট workspace + স্কিল

Workspace রুট: `~/.zeroclaw/workspace/` (কনফিগের মাধ্যমে কনফিগারযোগ্য)।

ইনজেক্ট করা প্রম্পট ফাইল:
- `IDENTITY.md` — এজেন্টের ব্যক্তিত্ব এবং ভূমিকা
- `USER.md` — ব্যবহারকারীর প্রসঙ্গ এবং পছন্দ
- `MEMORY.md` — দীর্ঘমেয়াদী তথ্য এবং শিক্ষা
- `AGENTS.md` — সেশন কনভেনশন এবং ইনিশিয়ালাইজেশন নিয়ম
- `SOUL.md` — মূল পরিচয় এবং পরিচালন নীতি

স্কিল: `~/.zeroclaw/workspace/skills/<skill>/SKILL.md` বা `SKILL.toml`।

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

## CLI কমান্ড

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

সম্পূর্ণ কমান্ড রেফারেন্স: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## পূর্বশর্ত

<details>
<summary><strong>Windows</strong></summary>

#### প্রয়োজনীয়

1. **Visual Studio Build Tools** (MSVC লিঙ্কার এবং Windows SDK প্রদান করে):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    ইনস্টলেশনের সময় (বা Visual Studio Installer এর মাধ্যমে), **"Desktop development with C++"** ওয়ার্কলোড নির্বাচন করুন।

2. **Rust টুলচেইন:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    ইনস্টলেশনের পরে, একটি নতুন টার্মিনাল খুলুন এবং `rustup default stable` চালান স্থিতিশীল টুলচেইন সক্রিয় করতে।

3. **যাচাই করুন** উভয়ই কাজ করছে:
    ```powershell
    rustc --version
    cargo --version
    ```

#### ঐচ্ছিক

- **Docker Desktop** — শুধুমাত্র [Docker স্যান্ডবক্সড রানটাইম](#রানটাইম-সাপোর্ট-বর্তমান) (`runtime.kind = "docker"`) ব্যবহার করলে প্রয়োজন। `winget install Docker.DockerDesktop` দিয়ে ইনস্টল করুন।

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### প্রয়োজনীয়

1. **বিল্ড এসেনশিয়ালস:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Xcode Command Line Tools ইনস্টল করুন: `xcode-select --install`

2. **Rust টুলচেইন:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    বিস্তারিতের জন্য [rustup.rs](https://rustup.rs) দেখুন।

3. **যাচাই করুন** উভয়ই কাজ করছে:
    ```bash
    rustc --version
    cargo --version
    ```

#### এক-লাইন ইনস্টলার

অথবা উপরের ধাপগুলো এড়িয়ে একটি কমান্ডে সবকিছু (সিস্টেম deps, Rust, ZeroClaw) ইনস্টল করুন:

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

#### কম্পাইলেশন রিসোর্স প্রয়োজনীয়তা

সোর্স থেকে বিল্ড করতে ফলাফল বাইনারি চালানোর চেয়ে বেশি রিসোর্স প্রয়োজন:

| রিসোর্স | ন্যূনতম | প্রস্তাবিত |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **ফ্রি ডিস্ক** | 6 GB    | 10 GB+      |

আপনার হোস্ট ন্যূনতমের নিচে হলে, প্রি-বিল্ট বাইনারি ব্যবহার করুন:

```bash
./install.sh --prefer-prebuilt
```

সোর্স ফলব্যাক ছাড়া শুধুমাত্র বাইনারি ইনস্টল করতে:

```bash
./install.sh --prebuilt-only
```

#### ঐচ্ছিক

- **Docker** — শুধুমাত্র [Docker স্যান্ডবক্সড রানটাইম](#রানটাইম-সাপোর্ট-বর্তমান) (`runtime.kind = "docker"`) ব্যবহার করলে প্রয়োজন। আপনার প্যাকেজ ম্যানেজার বা [docker.com](https://docs.docker.com/engine/install/) থেকে ইনস্টল করুন।

> **নোট:** ডিফল্ট `cargo build --release` পিক কম্পাইল প্রেশার কমাতে `codegen-units=1` ব্যবহার করে। শক্তিশালী মেশিনে দ্রুত বিল্ডের জন্য, `cargo build --profile release-fast` ব্যবহার করুন।

</details>

<!-- markdownlint-enable MD001 MD024 -->

### প্রি-বিল্ট বাইনারি

রিলিজ অ্যাসেট প্রকাশিত হয়:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

সর্বশেষ অ্যাসেট ডাউনলোড করুন:
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>

## ডকুমেন্টেশন

অনবোর্ডিং প্রবাহের পরে এবং গভীর রেফারেন্স চাইলে এগুলো ব্যবহার করুন।

- নেভিগেশন এবং "কোথায় কী" এর জন্য [ডকুমেন্টেশন ইনডেক্স](docs/README.md) দিয়ে শুরু করুন।
- সম্পূর্ণ সিস্টেম মডেলের জন্য [আর্কিটেকচার ওভারভিউ](docs/architecture.md) পড়ুন।
- প্রতিটি কী এবং উদাহরণ প্রয়োজন হলে [কনফিগারেশন রেফারেন্স](docs/reference/api/config-reference.md) ব্যবহার করুন।
- [অপারেশনাল রানবুক](docs/ops/operations-runbook.md) অনুযায়ী Gateway চালান।
- গাইডেড সেটআপের জন্য [ZeroClaw Onboard](#দ্রুত-শুরু) অনুসরণ করুন।
- [সমস্যা সমাধান গাইড](docs/ops/troubleshooting.md) দিয়ে সাধারণ ব্যর্থতা ডিবাগ করুন।
- কিছু এক্সপোজ করার আগে [নিরাপত্তা নির্দেশনা](docs/security/README.md) পর্যালোচনা করুন।

### রেফারেন্স ডকুমেন্টেশন

- ডকুমেন্টেশন হাব: [docs/README.md](docs/README.md)
- একীভূত ডকুমেন্টেশন TOC: [docs/SUMMARY.md](docs/SUMMARY.md)
- কমান্ড রেফারেন্স: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- কনফিগ রেফারেন্স: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- প্রদানকারী রেফারেন্স: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- চ্যানেল রেফারেন্স: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- অপারেশনস রানবুক: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- সমস্যা সমাধান: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### সহযোগিতা ডকুমেন্টেশন

- অবদান গাইড: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR ওয়ার্কফ্লো নীতি: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI ওয়ার্কফ্লো গাইড: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- পর্যালোচক প্লেবুক: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- নিরাপত্তা প্রকাশ নীতি: [SECURITY.md](SECURITY.md)
- ডকুমেন্টেশন টেমপ্লেট: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### ডিপ্লয়মেন্ট + অপারেশন

- নেটওয়ার্ক ডিপ্লয়মেন্ট গাইড: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- প্রক্সি এজেন্ট প্লেবুক: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- হার্ডওয়্যার গাইড: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

ZeroClaw smooth crab 🦀 এর জন্য তৈরি হয়েছিল, একটি দ্রুত এবং দক্ষ AI সহকারী। Argenis De La Rosa এবং সম্প্রদায় দ্বারা নির্মিত।

- [zeroclawlabs.ai](https://zeroclawlabs.ai)
- [@zeroclawlabs](https://x.com/zeroclawlabs)

## ZeroClaw সমর্থন করুন

### 🙏 বিশেষ ধন্যবাদ

যে সম্প্রদায় এবং প্রতিষ্ঠানগুলো এই ওপেন-সোর্স কাজকে অনুপ্রাণিত এবং শক্তি দেয় তাদের প্রতি আন্তরিক ধন্যবাদ:

- **Harvard University** — বৌদ্ধিক কৌতূহল লালন এবং সম্ভাবনার সীমানা প্রসারিত করার জন্য।
- **MIT** — খোলা জ্ঞান, ওপেন সোর্স, এবং প্রযুক্তি সবার জন্য অ্যাক্সেসযোগ্য হওয়া উচিত এই বিশ্বাসের চ্যাম্পিয়ন হওয়ার জন্য।
- **Sundai Club** — সম্প্রদায়, শক্তি, এবং গুরুত্বপূর্ণ জিনিস তৈরির অদম্য চেষ্টার জন্য।
- **বিশ্ব এবং তার বাইরে** 🌍✨ — প্রতিটি অবদানকারী, স্বপ্নদ্রষ্টা, এবং নির্মাতার জন্য যারা ওপেন সোর্সকে ভালোর শক্তি বানাচ্ছে। এটি আপনার জন্য।

আমরা খোলামেলাভাবে তৈরি করছি কারণ সেরা ধারণাগুলো সর্বত্র থেকে আসে। আপনি যদি এটি পড়ছেন, আপনি এর অংশ। স্বাগতম। 🦀❤️

## অবদান

ZeroClaw এ নতুন? [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) লেবেলযুক্ত ইস্যু খুঁজুন — কিভাবে শুরু করতে হয় তা জানতে আমাদের [অবদান গাইড](CONTRIBUTING.md#first-time-contributors) দেখুন। AI/vibe-coded PR স্বাগত! 🤖

[CONTRIBUTING.md](CONTRIBUTING.md) এবং [CLA.md](docs/contributing/cla.md) দেখুন। একটি trait বাস্তবায়ন করুন, PR জমা দিন:

- CI ওয়ার্কফ্লো গাইড: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- নতুন `Provider` → `src/providers/`
- নতুন `Channel` → `src/channels/`
- নতুন `Observer` → `src/observability/`
- নতুন `Tool` → `src/tools/`
- নতুন `Memory` → `src/memory/`
- নতুন `Tunnel` → `src/tunnel/`
- নতুন `Peripheral` → `src/peripherals/`
- নতুন `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ অফিসিয়াল রিপোজিটরি এবং ছদ্মবেশ সতর্কতা

**এটিই একমাত্র অফিসিয়াল ZeroClaw রিপোজিটরি:**

> https://github.com/zeroclaw-labs/zeroclaw

অন্য কোনো রিপোজিটরি, সংগঠন, ডোমেইন, বা প্যাকেজ যা "ZeroClaw" বলে দাবি করে বা ZeroClaw Labs এর সাথে সংযুক্তি ইঙ্গিত করে তা **অননুমোদিত এবং এই প্রকল্পের সাথে সম্পর্কিত নয়**। পরিচিত অননুমোদিত ফর্ক [TRADEMARK.md](docs/maintainers/trademark.md) তে তালিকাভুক্ত করা হবে।

আপনি ছদ্মবেশ বা ট্রেডমার্ক অপব্যবহারের সম্মুখীন হলে, অনুগ্রহ করে [একটি ইস্যু খুলুন](https://github.com/zeroclaw-labs/zeroclaw/issues)।

---

## লাইসেন্স

ZeroClaw সর্বোচ্চ উন্মুক্ততা এবং অবদানকারী সুরক্ষার জন্য দ্বৈত-লাইসেন্সপ্রাপ্ত:

| লাইসেন্স | ব্যবহারের ক্ষেত্র |
|---|---|
| [MIT](LICENSE-MIT) | ওপেন-সোর্স, গবেষণা, একাডেমিক, ব্যক্তিগত ব্যবহার |
| [Apache 2.0](LICENSE-APACHE) | পেটেন্ট সুরক্ষা, প্রাতিষ্ঠানিক, বাণিজ্যিক ডিপ্লয়মেন্ট |

আপনি যেকোনো লাইসেন্স বেছে নিতে পারেন। **অবদানকারীরা স্বয়ংক্রিয়ভাবে উভয়ের অধীনে অধিকার প্রদান করে** — সম্পূর্ণ অবদানকারী চুক্তির জন্য [CLA.md](docs/contributing/cla.md) দেখুন।

### ট্রেডমার্ক

**ZeroClaw** নাম এবং লোগো ZeroClaw Labs এর ট্রেডমার্ক। এই লাইসেন্স সমর্থন বা সংযুক্তি ইঙ্গিত করতে এগুলো ব্যবহারের অনুমতি দেয় না। অনুমোদিত এবং নিষিদ্ধ ব্যবহারের জন্য [TRADEMARK.md](docs/maintainers/trademark.md) দেখুন।

### অবদানকারী সুরক্ষা

- আপনি আপনার অবদানের **কপিরাইট ধরে রাখেন**
- **পেটেন্ট অনুদান** (Apache 2.0) আপনাকে অন্যান্য অবদানকারীদের পেটেন্ট দাবি থেকে রক্ষা করে
- আপনার অবদান কমিট ইতিহাস এবং [NOTICE](NOTICE) এ **স্থায়ীভাবে বিশেষিত**
- অবদান করে কোনো ট্রেডমার্ক অধিকার হস্তান্তরিত হয় না

---

**ZeroClaw** — শূন্য ওভারহেড। শূন্য আপস। যেকোনো জায়গায় ডিপ্লয় করুন। যেকিছু বিনিময় করুন। 🦀

## অবদানকারীরা

<a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
</a>

এই তালিকা GitHub অবদানকারী গ্রাফ থেকে তৈরি হয় এবং স্বয়ংক্রিয়ভাবে আপডেট হয়।

## স্টার ইতিহাস

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
