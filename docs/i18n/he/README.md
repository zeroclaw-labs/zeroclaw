<p align="center">
  <img src="../../assets/quantclaw-banner.png" alt="QuantClaw" width="600" />
</p>

<h1 align="center">🦀 QuantClaw — עוזר AI אישי</h1>

<p align="center">
  <strong>אפס תקורה. אפס פשרות. 100% Rust. 100% אגנוסטי.</strong><br>
  ⚡️ <strong>רץ על חומרה של $10 עם פחות מ-5MB RAM: זה 99% פחות זיכרון מ-OpenClaw ו-98% זול יותר מ-Mac mini!</strong>
</p>

<p align="center">
נבנה על ידי סטודנטים וחברים מקהילות Harvard, MIT ו-Sundai.Club.
</p>

<p align="center">
  🌐 <strong>שפות:</strong>
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

QuantClaw הוא עוזר AI אישי שאתה מריץ על המכשירים שלך. הוא עונה לך בערוצים שאתה כבר משתמש בהם (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, ועוד). יש לו לוח בקרה אינטרנטי לשליטה בזמן אמת ויכול להתחבר להתקנים היקפיים (ESP32, STM32, Arduino, Raspberry Pi). ה-Gateway הוא רק מישור הבקרה — המוצר הוא העוזר.

אם אתה רוצה עוזר אישי למשתמש יחיד שמרגיש מקומי, מהיר ותמיד פעיל, זה הוא.

<p align="center">
  <a href="https://quantspeed.ai">אתר</a> ·
  <a href="docs/README.md">תיעוד</a> ·
  <a href="docs/architecture.md">ארכיטקטורה</a> ·
  <a href="#התחלה-מהירה">התחלה</a> ·
  <a href="#מיגרציה-מ-openclaw">מיגרציה מ-OpenClaw</a> ·
  <a href="docs/ops/troubleshooting.md">פתרון בעיות</a> ·
</p>

> **הגדרה מועדפת:** הרץ `quantclaw onboard` בטרמינל שלך. QuantClaw Onboard מנחה אותך שלב אחר שלב בהגדרת ה-gateway, סביבת העבודה, הערוצים והספק. זהו נתיב ההגדרה המומלץ ועובד על macOS, Linux ו-Windows (דרך WSL2). התקנה חדשה? התחל כאן: [התחלה](#התחלה-מהירה)

### אימות מנוי (OAuth)

- **OpenAI Codex** (מנוי ChatGPT)
- **Gemini** (Google OAuth)
- **Anthropic** (מפתח API או אסימון אימות)

הערה על מודלים: בעוד שספקים/מודלים רבים נתמכים, לחוויה הטובה ביותר השתמש במודל הדור האחרון החזק ביותר הזמין לך. ראה [הכניסה](#התחלה-מהירה).

הגדרות מודלים + CLI: [מדריך ספקים](docs/reference/api/providers-reference.md)
רוטציית פרופיל אימות (OAuth מול מפתחות API) + מעבר בכשל: [מעבר מודלים בכשל](docs/reference/api/providers-reference.md)

## התקנה (מומלץ)

סביבת ריצה: שרשרת כלים יציבה של Rust. בינארי יחיד, ללא תלויות סביבת ריצה.

### Homebrew (macOS/Linuxbrew)

```bash
brew install quantclaw
```

### התקנה בלחיצה אחת

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw
./install.sh
```

`quantclaw onboard` רץ אוטומטית לאחר ההתקנה כדי להגדיר את סביבת העבודה והספק שלך.

## התחלה מהירה (TL;DR)

מדריך מתחילים מלא (אימות, צימוד, ערוצים): [התחלה](docs/setup-guides/one-click-bootstrap.md)

```bash
# Install + onboard
./install.sh --api-key "sk-..." --provider openrouter

# Start the gateway (webhook server + web dashboard)
quantclaw gateway                # default: 127.0.0.1:42617
quantclaw gateway --port 0       # random port (security hardened)

# Talk to the assistant
quantclaw agent -m "Hello, QuantClaw!"

# Interactive mode
quantclaw agent

# Start full autonomous runtime (gateway + channels + cron + hands)
quantclaw daemon

# Check status
quantclaw status

# Run diagnostics
quantclaw doctor
```

משדרג? הרץ `quantclaw doctor` לאחר העדכון.

### מקוד מקור (פיתוח)

```bash
git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --locked
cargo install --path . --force --locked

quantclaw onboard
```

> **חלופת פיתוח (ללא התקנה גלובלית):** הוסף `cargo run --release --` לפני פקודות (דוגמה: `cargo run --release -- status`).

## מיגרציה מ-OpenClaw

QuantClaw יכול לייבא את סביבת העבודה, הזיכרון וההגדרות של OpenClaw שלך:

```bash
# Preview what will be migrated (safe, read-only)
quantclaw migrate openclaw --dry-run

# Run the migration
quantclaw migrate openclaw
```

זה מעביר את רשומות הזיכרון, קבצי סביבת העבודה וההגדרות מ-`~/.openclaw/` ל-`~/.quantclaw/`. ההגדרות מומרות אוטומטית מ-JSON ל-TOML.

## ברירות מחדל אבטחה (גישת DM)

QuantClaw מתחבר למשטחי הודעות אמיתיים. התייחס ל-DM נכנסים כקלט לא מהימן.

מדריך אבטחה מלא: [SECURITY.md](SECURITY.md)

התנהגות ברירת מחדל בכל הערוצים:

- **צימוד DM** (ברירת מחדל): שולחים לא מוכרים מקבלים קוד צימוד קצר והבוט לא מעבד את ההודעה שלהם.
- אשר עם: `quantclaw pairing approve <channel> <code>` (ואז השולח נוסף לרשימת היתרים מקומית).
- DM נכנסים ציבוריים דורשים הסכמה מפורשת ב-`config.toml`.
- הרץ `quantclaw doctor` כדי לחשוף מדיניות DM מסוכנת או שגויה.

**רמות אוטונומיה:**

| רמה | התנהגות |
|------|----------|
| `ReadOnly` | הסוכן יכול לצפות אבל לא לפעול |
| `Supervised` (ברירת מחדל) | הסוכן פועל עם אישור לפעולות בסיכון בינוני/גבוה |
| `Full` | הסוכן פועל באופן אוטונומי בגבולות המדיניות |

**שכבות ארגז חול:** בידוד סביבת עבודה, חסימת מעבר נתיבים, רשימות היתר לפקודות, נתיבים אסורים (`/etc`, `/root`, `~/.ssh`), הגבלת קצב (מקסימום פעולות/שעה, מגבלות עלות/יום).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 הודעות

השתמש בלוח זה להודעות חשובות (שינויים שוברים, ייעוץ אבטחה, חלונות תחזוקה וחוסמי שחרור).

| תאריך (UTC) | רמה | הודעה | פעולה |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _קריטי_ | אנחנו **לא מזוהים** עם `openagen/quantclaw`, `quantclaw.org` או `quantclaw.net`. הדומיינים `quantclaw.org` ו-`quantclaw.net` מפנים כרגע ל-fork `openagen/quantclaw`, ואותו דומיין/מאגר מתחזים לאתר/פרויקט הרשמי שלנו. | אל תסמוך על מידע, בינאריים, גיוס כספים או הודעות ממקורות אלה. השתמש רק ב[מאגר זה](https://github.com/quant-speed/quantclaw) ובחשבונות החברתיים המאומתים שלנו. |
| 2026-02-19 | _חשוב_ | Anthropic עדכנה את תנאי Authentication and Credential Use ב-2026-02-19. אסימוני Claude Code OAuth (Free, Pro, Max) מיועדים אך ורק ל-Claude Code ול-Claude.ai; שימוש באסימוני OAuth מ-Claude Free/Pro/Max בכל מוצר, כלי או שירות אחר (כולל Agent SDK) אינו מותר ועלול להפר את תנאי השירות לצרכן. | אנא הימנעו זמנית מאינטגרציות Claude Code OAuth כדי למנוע אובדן פוטנציאלי. סעיף מקורי: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## יתרונות עיקריים

- **סביבת ריצה קלה כברירת מחדל** — תהליכי CLI וסטטוס שגרתיים רצים במעטפת זיכרון של כמה מגה-בייט על בנייות שחרור.
- **פריסה חסכונית** — מתוכנן ללוחות של $10 ומופעי ענן קטנים, ללא תלויות סביבת ריצה כבדות.
- **התחלה קרה מהירה** — סביבת ריצה Rust בבינארי יחיד שומרת על הפעלת פקודות ודמון כמעט מיידית.
- **ארכיטקטורה ניידת** — בינארי אחד על ARM, x86 ו-RISC-V עם ספקים/ערוצים/כלים להחלפה.
- **Gateway מקומי-תחילה** — מישור בקרה יחיד לסשנים, ערוצים, כלים, cron, SOPs ואירועים.
- **תיבת דואר רב-ערוצית** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket, ועוד.
- **תזמור רב-סוכנים (Hands)** — נחילי סוכנים אוטונומיים הפועלים לפי לוח זמנים ומשתפרים עם הזמן.
- **נהלי הפעלה סטנדרטיים (SOPs)** — אוטומציית תהליכי עבודה מונעת אירועים עם MQTT, webhook, cron וטריגרים של התקנים היקפיים.
- **לוח בקרה אינטרנטי** — ממשק משתמש React 19 + Vite עם צ'אט בזמן אמת, דפדפן זיכרון, עורך הגדרות, מנהל cron ומפקח כלים.
- **התקנים היקפיים** — ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO דרך trait `Peripheral`.
- **כלים מדרגה ראשונה** — shell, קריאה/כתיבה/עריכת קבצים, git, שליפת/חיפוש אינטרנט, MCP, Jira, Notion, Google Workspace, ו-70+ נוספים.
- **הוקים של מחזור חיים** — יירוט ושינוי קריאות LLM, הרצות כלים והודעות בכל שלב.
- **פלטפורמת מיומנויות** — מיומנויות מובנות, קהילתיות וסביבת עבודה עם ביקורת אבטחה.
- **תמיכה במנהרות** — Cloudflare, Tailscale, ngrok, OpenVPN ומנהרות מותאמות לגישה מרחוק.

### למה צוותים בוחרים ב-QuantClaw

- **קל כברירת מחדל:** בינארי Rust קטן, הפעלה מהירה, טביעת רגל זיכרון נמוכה.
- **מאובטח מהתכנון:** צימוד, ארגז חול מחמיר, רשימות היתר מפורשות, תיחום סביבת עבודה.
- **ניתן להחלפה מלאה:** מערכות ליבה הן traits (ספקים, ערוצים, כלים, זיכרון, מנהרות).
- **ללא נעילת ספק:** תמיכה בספקים תואמי OpenAI + נקודות קצה מותאמות הניתנות לחיבור.

## תמונת מצב של ביצועים (QuantClaw מול OpenClaw, ניתן לשחזור)

מדד מהיר על מכונה מקומית (macOS arm64, פברואר 2026) מנורמל לחומרת edge בתדר 0.8GHz.

|                           | OpenClaw      | NanoBot        | PicoClaw        | QuantClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **שפה**              | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **הפעלה (ליבת 0.8GHz)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **גודל בינארי**           | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **עלות**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **כל חומרה $10** |

> הערות: תוצאות QuantClaw נמדדו על בנייות שחרור באמצעות `/usr/bin/time -l`. OpenClaw דורש סביבת ריצה Node.js (בדרך כלל ~390MB תקורת זיכרון נוספת), בעוד NanoBot דורש סביבת ריצה Python. PicoClaw ו-QuantClaw הם בינאריים סטטיים. נתוני ה-RAM למעלה הם זיכרון סביבת ריצה; דרישות קומפילציה בזמן בנייה גבוהות יותר.

<p align="center">
  <img src="docs/assets/quantclaw-comparison.jpeg" alt="QuantClaw vs OpenClaw Comparison" width="800" />
</p>

### מדידה מקומית ניתנת לשחזור

```bash
cargo build --release
ls -lh target/release/quantclaw

/usr/bin/time -l target/release/quantclaw --help
/usr/bin/time -l target/release/quantclaw status
```

## כל מה שבנינו עד כה

### פלטפורמת ליבה

- Gateway HTTP/WS/SSE מישור בקרה עם סשנים, נוכחות, הגדרות, cron, webhooks, לוח בקרה אינטרנטי וצימוד.
- משטח CLI: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- לולאת תזמור סוכן עם שליחת כלים, בניית פרומפט, סיווג הודעות וטעינת זיכרון.
- מודל סשנים עם אכיפת מדיניות אבטחה, רמות אוטונומיה ושער אישור.
- מעטפת ספק עמידה עם מעבר בכשל, ניסיון חוזר וניתוב מודלים על פני 20+ ממשקי LLM.

### ערוצים

ערוצים: WhatsApp (מקורי), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

מוגבלי-תכונה: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### לוח בקרה אינטרנטי

לוח בקרה React 19 + Vite 6 + Tailwind CSS 4 מוגש ישירות מה-Gateway:

- **לוח בקרה** — סקירת מערכת, מצב בריאות, זמן פעילות, מעקב עלויות
- **צ'אט סוכן** — צ'אט אינטראקטיבי עם הסוכן
- **זיכרון** — דפדוף וניהול רשומות זיכרון
- **הגדרות** — צפייה ועריכת הגדרות
- **Cron** — ניהול משימות מתוזמנות
- **כלים** — דפדוף בכלים זמינים
- **יומנים** — צפייה ביומני פעילות הסוכן
- **עלות** — שימוש בטוקנים ומעקב עלויות
- **דוקטור** — אבחון בריאות המערכת
- **אינטגרציות** — מצב אינטגרציות והגדרה
- **צימוד** — ניהול צימוד מכשירים

### יעדי קושחה

| יעד | פלטפורמה | מטרה |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | סוכן היקפי אלחוטי |
| ESP32-UI | ESP32 + Display | סוכן עם ממשק חזותי |
| STM32 Nucleo | STM32 (ARM Cortex-M) | התקן היקפי תעשייתי |
| Arduino | Arduino | גשר חיישן/מפעיל בסיסי |
| Uno Q Bridge | Arduino Uno | גשר סריאלי לסוכן |

### כלים + אוטומציה

- **ליבה:** shell, קריאה/כתיבה/עריכת קבצים, פעולות git, חיפוש glob, חיפוש תוכן
- **אינטרנט:** שליטה בדפדפן, web fetch, web search, צילום מסך, מידע תמונה, קריאת PDF
- **אינטגרציות:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** מעטפת כלי Model Context Protocol + סטים של כלים מושהים
- **תזמון:** cron add/remove/update/run, כלי תזמון
- **זיכרון:** recall, store, forget, knowledge, project intel
- **מתקדם:** delegate (סוכן-לסוכן), swarm, החלפת/ניתוב מודל, פעולות אבטחה, פעולות ענן
- **חומרה:** מידע לוח, מפת זיכרון, קריאת זיכרון (מוגבל-תכונה)

### סביבת ריצה + אבטחה

- **רמות אוטונומיה:** ReadOnly, Supervised (ברירת מחדל), Full.
- **ארגז חול:** בידוד סביבת עבודה, חסימת מעבר נתיבים, רשימות היתר לפקודות, נתיבים אסורים, Landlock (Linux), Bubblewrap.
- **הגבלת קצב:** מקסימום פעולות בשעה, מקסימום עלות ביום (ניתן להגדרה).
- **שער אישור:** אישור אינטראקטיבי לפעולות בסיכון בינוני/גבוה.
- **עצירת חירום:** יכולת כיבוי חירום.
- **129+ מבחני אבטחה** ב-CI אוטומטי.

### תפעול + אריזה

- לוח בקרה אינטרנטי מוגש ישירות מה-Gateway.
- תמיכה במנהרות: Cloudflare, Tailscale, ngrok, OpenVPN, פקודה מותאמת.
- מתאם סביבת ריצה Docker להרצה בקונטיינרים.
- CI/CD: בטא (אוטומטי בדחיפה) → יציב (שליחה ידנית) → Docker, crates.io, Scoop, AUR, Homebrew, ציוץ.
- בינאריים מוכנים מראש ל-Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64).


## הגדרות

מינימלי `~/.quantclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

מדריך הגדרות מלא: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### הגדרת ערוצים

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

### הגדרת מנהרות

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

פרטים: [מדריך ערוצים](docs/reference/api/channels-reference.md) · [מדריך הגדרות](docs/reference/api/config-reference.md)

### תמיכה בסביבת ריצה (נוכחי)

- **`native`** (ברירת מחדל) — הרצת תהליך ישירה, הנתיב המהיר ביותר, אידיאלי לסביבות מהימנות.
- **`docker`** — בידוד קונטיינר מלא, מדיניות אבטחה נאכפת, דורש Docker.

הגדר `runtime.kind = "docker"` לארגז חול מחמיר או בידוד רשת.

## אימות מנוי (OpenAI Codex / Claude Code / Gemini)

QuantClaw תומך בפרופילי אימות מקוריים למנוי (רב-חשבוני, מוצפן במנוחה).

- קובץ אחסון: `~/.quantclaw/auth-profiles.json`
- מפתח הצפנה: `~/.quantclaw/.secret_key`
- פורמט מזהה פרופיל: `<provider>:<profile_name>` (דוגמה: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT subscription)
quantclaw auth login --provider openai-codex --device-code

# Gemini OAuth
quantclaw auth login --provider gemini --profile default

# Anthropic setup-token
quantclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Check / refresh / switch profile
quantclaw auth status
quantclaw auth refresh --provider openai-codex --profile default
quantclaw auth use --provider openai-codex --profile work

# Run the agent with subscription auth
quantclaw agent --provider openai-codex -m "hello"
quantclaw agent --provider anthropic -m "hello"
```

## סביבת עבודה של הסוכן + מיומנויות

שורש סביבת עבודה: `~/.quantclaw/workspace/` (ניתן להגדרה דרך ההגדרות).

קבצי פרומפט מוזרקים:
- `IDENTITY.md` — אישיות ותפקיד הסוכן
- `USER.md` — הקשר והעדפות המשתמש
- `MEMORY.md` — עובדות ולקחים לטווח ארוך
- `AGENTS.md` — מוסכמות סשן וכללי אתחול
- `SOUL.md` — זהות ליבה ועקרונות הפעלה

מיומנויות: `~/.quantclaw/workspace/skills/<skill>/SKILL.md` או `SKILL.toml`.

```bash
# List installed skills
quantclaw skills list

# Install from git
quantclaw skills install https://github.com/user/my-skill.git

# Security audit before install
quantclaw skills audit https://github.com/user/my-skill.git

# Remove a skill
quantclaw skills remove my-skill
```

## פקודות CLI

```bash
# Workspace management
quantclaw onboard              # Guided setup wizard
quantclaw status               # Show daemon/agent status
quantclaw doctor               # Run system diagnostics

# Gateway + daemon
quantclaw gateway              # Start gateway server (127.0.0.1:42617)
quantclaw daemon               # Start full autonomous runtime

# Agent
quantclaw agent                # Interactive chat mode
quantclaw agent -m "message"   # Single message mode

# Service management
quantclaw service install      # Install as OS service (launchd/systemd)
quantclaw service start|stop|restart|status

# Channels
quantclaw channel list         # List configured channels
quantclaw channel doctor       # Check channel health
quantclaw channel bind-telegram 123456789

# Cron + scheduling
quantclaw cron list            # List scheduled jobs
quantclaw cron add "*/5 * * * *" --prompt "Check system health"
quantclaw cron remove <id>

# Memory
quantclaw memory list          # List memory entries
quantclaw memory get <key>     # Retrieve a memory
quantclaw memory stats         # Memory statistics

# Auth profiles
quantclaw auth login --provider <name>
quantclaw auth status
quantclaw auth use --provider <name> --profile <profile>

# Hardware peripherals
quantclaw hardware discover    # Scan for connected devices
quantclaw peripheral list      # List connected peripherals
quantclaw peripheral flash     # Flash firmware to device

# Migration
quantclaw migrate openclaw --dry-run
quantclaw migrate openclaw

# Shell completions
source <(quantclaw completions bash)
quantclaw completions zsh > ~/.zfunc/_quantclaw
```

מדריך פקודות מלא: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## דרישות מקדימות

<details>
<summary><strong>Windows</strong></summary>

#### נדרש

1. **Visual Studio Build Tools** (מספק את מקשר MSVC ו-Windows SDK):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    במהלך ההתקנה (או דרך Visual Studio Installer), בחר את עומס העבודה **"Desktop development with C++"**.

2. **שרשרת כלים Rust:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    לאחר ההתקנה, פתח טרמינל חדש והרץ `rustup default stable` כדי לוודא ששרשרת הכלים היציבה פעילה.

3. **אמת** ששניהם עובדים:
    ```powershell
    rustc --version
    cargo --version
    ```

#### אופציונלי

- **Docker Desktop** — נדרש רק אם משתמשים ב[סביבת ריצה Docker בארגז חול](#תמיכה-בסביבת-ריצה-נוכחי) (`runtime.kind = "docker"`). התקן דרך `winget install Docker.DockerDesktop`.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### נדרש

1. **כלי בנייה:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** התקן Xcode Command Line Tools: `xcode-select --install`

2. **שרשרת כלים Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    ראה [rustup.rs](https://rustup.rs) לפרטים.

3. **אמת** ששניהם עובדים:
    ```bash
    rustc --version
    cargo --version
    ```

#### מתקין בשורה אחת

או דלג על השלבים למעלה והתקן הכל (תלויות מערכת, Rust, QuantClaw) בפקודה אחת:

```bash
curl -LsSf https://raw.githubusercontent.com/quant-speed/quantclaw/master/install.sh | bash
```

#### דרישות משאבי קומפילציה

בנייה מקוד מקור דורשת יותר משאבים מהרצת הבינארי המתקבל:

| משאב | מינימום | מומלץ |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **דיסק פנוי** | 6 GB    | 10 GB+      |

אם המארח שלך מתחת למינימום, השתמש בבינאריים מוכנים מראש:

```bash
./install.sh --prefer-prebuilt
```

כדי לדרוש התקנת בינארי בלבד ללא חלופת מקור:

```bash
./install.sh --prebuilt-only
```

#### אופציונלי

- **Docker** — נדרש רק אם משתמשים ב[סביבת ריצה Docker בארגז חול](#תמיכה-בסביבת-ריצה-נוכחי) (`runtime.kind = "docker"`). התקן דרך מנהל החבילות שלך או [docker.com](https://docs.docker.com/engine/install/).

> **הערה:** ברירת המחדל `cargo build --release` משתמשת ב-`codegen-units=1` כדי להפחית לחץ קומפילציה שיא. לבנייות מהירות יותר על מכונות חזקות, השתמש ב-`cargo build --profile release-fast`.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### בינאריים מוכנים מראש

נכסי שחרור מפורסמים עבור:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

הורד את הנכסים האחרונים מ:
<https://github.com/quant-speed/quantclaw/releases/latest>

## תיעוד

השתמש באלה כשעברת את תהליך ההכניסה ורוצה את המדריך המעמיק יותר.

- התחל עם [אינדקס התיעוד](docs/README.md) לניווט ו"מה נמצא איפה."
- קרא את [סקירת הארכיטקטורה](docs/architecture.md) למודל המערכת המלא.
- השתמש ב[מדריך ההגדרות](docs/reference/api/config-reference.md) כשאתה צריך כל מפתח ודוגמה.
- הפעל את ה-Gateway לפי הספר עם [מדריך התפעול](docs/ops/operations-runbook.md).
- עקוב אחרי [QuantClaw Onboard](#התחלה-מהירה) להגדרה מונחית.
- אבחן כשלים נפוצים עם [מדריך פתרון בעיות](docs/ops/troubleshooting.md).
- סקור את [הנחיות האבטחה](docs/security/README.md) לפני חשיפת משהו.

### תיעוד מדריכים

- מרכז תיעוד: [docs/README.md](docs/README.md)
- תוכן עניינים מאוחד: [docs/SUMMARY.md](docs/SUMMARY.md)
- מדריך פקודות: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- מדריך הגדרות: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- מדריך ספקים: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- מדריך ערוצים: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- מדריך תפעול: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- פתרון בעיות: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### תיעוד שיתוף פעולה

- מדריך תרומה: [CONTRIBUTING.md](CONTRIBUTING.md)
- מדיניות תהליך PR: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- מדריך תהליך CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- מדריך סוקר: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- מדיניות חשיפת אבטחה: [SECURITY.md](SECURITY.md)
- תבנית תיעוד: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### פריסה + תפעול

- מדריך פריסת רשת: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- מדריך סוכן פרוקסי: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- מדריכי חומרה: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

QuantClaw נבנה עבור ה-smooth crab 🦀, עוזר AI מהיר ויעיל. נבנה על ידי Argenis De La Rosa והקהילה.

- [quantspeed.ai](https://quantspeed.ai)
- [@quantspeed](https://x.com/quantspeed)

## תמוך ב-QuantClaw

אם QuantClaw עוזר לעבודה שלך ואתה רוצה לתמוך בפיתוח המתמשך, אתה יכול לתרום כאן:

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 תודה מיוחדת

תודה מכל הלב לקהילות ולמוסדות שמעוררים השראה ומניעים את עבודת הקוד הפתוח הזו:

- **Harvard University** — על טיפוח סקרנות אינטלקטואלית ודחיפת גבולות האפשרי.
- **MIT** — על קידום ידע פתוח, קוד פתוח והאמונה שטכנולוגיה צריכה להיות נגישה לכולם.
- **Sundai Club** — על הקהילה, האנרגיה והמאמץ הבלתי פוסק לבנות דברים שחשובים.
- **העולם ומעבר** 🌍✨ — לכל תורם, חולם ובונה שם שהופך קוד פתוח לכוח לטובה. זה בשבילכם.

אנחנו בונים בגלוי כי הרעיונות הטובים ביותר מגיעים מכל מקום. אם אתה קורא את זה, אתה חלק מזה. ברוך הבא. 🦀❤️

## תרומה

חדש ב-QuantClaw? חפש בעיות עם התווית [`good first issue`](https://github.com/quant-speed/quantclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — ראה את [מדריך התרומה](CONTRIBUTING.md#first-time-contributors) שלנו כדי להתחיל. PR של AI/vibe-coded מתקבלים בברכה! 🤖

ראה [CONTRIBUTING.md](CONTRIBUTING.md) ו-[CLA.md](docs/contributing/cla.md). ממש trait, שלח PR:

- מדריך תהליך CI: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- `Provider` חדש → `src/providers/`
- `Channel` חדש → `src/channels/`
- `Observer` חדש → `src/observability/`
- `Tool` חדש → `src/tools/`
- `Memory` חדש → `src/memory/`
- `Tunnel` חדש → `src/tunnel/`
- `Peripheral` חדש → `src/peripherals/`
- `Skill` חדש → `~/.quantclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ מאגר רשמי ואזהרת התחזות

**זהו מאגר QuantClaw הרשמי היחיד:**

> https://github.com/quant-speed/quantclaw

כל מאגר, ארגון, דומיין או חבילה אחרים הטוענים להיות "QuantClaw" או מרמזים על שיוך ל-QuantClaw Labs הם **לא מורשים ולא מזוהים עם פרויקט זה**. פורקים לא מורשים ידועים ירשמו ב-[TRADEMARK.md](docs/maintainers/trademark.md).

אם אתה נתקל בהתחזות או שימוש לרעה בסימן מסחרי, אנא [פתח issue](https://github.com/quant-speed/quantclaw/issues).

---

## רישיון

QuantClaw מורשה ברישיון כפול לפתיחות מקסימלית והגנה על תורמים:

| רישיון | מקרה שימוש |
|---|---|
| [MIT](LICENSE-MIT) | קוד פתוח, מחקר, אקדמי, שימוש אישי |
| [Apache 2.0](LICENSE-APACHE) | הגנת פטנטים, מוסדי, פריסה מסחרית |

אתה יכול לבחור כל רישיון. **תורמים מעניקים זכויות באופן אוטומטי תחת שניהם** — ראה [CLA.md](docs/contributing/cla.md) להסכם התורם המלא.

### סימן מסחרי

השם והלוגו של **QuantClaw** הם סימנים מסחריים של QuantClaw Labs. רישיון זה אינו מעניק הרשאה להשתמש בהם כדי לרמוז על תמיכה או שיוך. ראה [TRADEMARK.md](docs/maintainers/trademark.md) לשימושים מותרים ואסורים.

### הגנות על תורמים

- אתה **שומר על זכויות יוצרים** על תרומותיך
- **הענקת פטנט** (Apache 2.0) מגנה עליך מתביעות פטנט של תורמים אחרים
- תרומותיך **מיוחסות באופן קבוע** בהיסטוריית הקומיטים וב-[NOTICE](NOTICE)
- לא מועברות זכויות סימן מסחרי על ידי תרומה

---

**QuantClaw** — אפס תקורה. אפס פשרות. פרוס בכל מקום. החלף הכל. 🦀

## תורמים

<a href="https://github.com/quant-speed/quantclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=quant-speed/quantclaw" alt="QuantClaw contributors" />
</a>

רשימה זו נוצרת מגרף התורמים של GitHub ומתעדכנת אוטומטית.

## היסטוריית כוכבים

<p align="center">
  <a href="https://www.star-history.com/#quant-speed/quantclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=quant-speed/quantclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
