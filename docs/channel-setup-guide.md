# Channel Setup Guide / 채널 세팅 가이드

Connect MoA to your favorite messaging platforms in minutes.
MoA를 좋아하는 메시징 플랫폼에 몇 분 만에 연결하세요.

---

## Quick Start / 빠른 시작

### 1. Choose Your Channel / 채널 선택

| Channel | Difficulty | Setup Time |
|---------|-----------|------------|
| **Web Chat** | Easy | 1 min |
| **Telegram** | Easy | 3 min |
| **Discord** | Easy | 5 min |
| **KakaoTalk** | Medium | 10 min |
| **Slack** | Medium | 10 min |
| **LINE** | Medium | 10 min |
| **WhatsApp** | Medium | 15 min |
| **Email** | Easy | 5 min |

### 2. Get Your API Keys / API 키 받기

Each channel requires platform-specific API keys. Follow the guides below.

---

## Web Chat / 웹 채팅

The easiest way to start. No external accounts needed.

```bash
# Start the gateway
zeroclaw gateway

# Open in browser
# http://localhost:42617
```

Enter the pairing code shown in your terminal. That's it!

터미널에 표시된 페어링 코드를 입력하세요. 끝!

---

## KakaoTalk / 카카오톡

### Prerequisites / 사전 준비
- Kakao account (카카오 계정)
- Business channel (비즈니스 채널) — required for Channel API

### Step 1: Register App / 앱 등록

1. Go to [Kakao Developers](https://developers.kakao.com)
2. Click "Add Application" (애플리케이션 추가)
3. Enter app name (e.g., "MoA")
4. Note your **REST API Key** and **Admin Key** from App Settings → App Keys

### Step 2: Create Channel / 채널 생성

1. Go to [KakaoTalk Channel Manager](https://center-pf.kakao.com)
2. Create a new channel or select existing one
3. Link the channel to your Developers app

### Step 3: Configure Chatbot / 챗봇 설정

1. Go to [Kakao i Open Builder](https://chatbot.kakao.com)
2. Create a new bot
3. Register skill server URL: `https://your-domain:8787/kakao/webhook`
4. Connect skill to a scenario block

> **Tip**: Use [ngrok](https://ngrok.com) or `zeroclaw tunnel` for local testing:
> ```bash
> ngrok http 8787
> # Use the ngrok URL as your webhook URL
> ```

### Step 4: Configure MoA / MoA 설정

**Option A: Config file** (`~/.zeroclaw/config.toml`)
```toml
[channels_config.kakao]
rest_api_key = "YOUR_REST_API_KEY"
admin_key = "YOUR_ADMIN_KEY"
allowed_users = ["*"]
port = 8787
# Optional — enables the 단톡방 one-tap share-back button on AI replies.
# Get this from Kakao Developers → 앱 설정 → 일반 → JavaScript 키.
javascript_app_key = "YOUR_KAKAO_JS_APP_KEY"
```

**Option B: Environment variables**
```bash
export ZEROCLAW_KAKAO_REST_API_KEY="YOUR_REST_API_KEY"
export ZEROCLAW_KAKAO_ADMIN_KEY="YOUR_ADMIN_KEY"
export ZEROCLAW_KAKAO_ALLOWED_USERS="*"
```

### Step 5: Start and Test / 시작 및 테스트

```bash
zeroclaw gateway
```

Add the channel as a friend in KakaoTalk and send a message!

카카오톡에서 채널을 친구 추가하고 메시지를 보내세요!

### Available Commands / 사용 가능한 명령

| Command | Description |
|---------|-------------|
| `/status` | Check agent status / 에이전트 상태 확인 |
| `/help` | Show commands / 명령어 목록 |
| `/memory <query>` | Search memory / 메모리 검색 |
| `/remember <text>` | Save to memory / 메모리 저장 |
| `/forget <key>` | Delete memory / 메모리 삭제 |
| `/cron` | List scheduled tasks / 예약 작업 목록 |
| `/case start <라벨>` | Pin a case session / 사건 시작 — also `/사건 시작` |
| `/case end` | End the active case / 사건 종료 |
| `/case current` | Show active case / 현재 사건 보기 |
| `/case list` | List active cases / 내 사건 목록 |
| `/mode current` | Show channel mode / 현재 모드 — also `/모드` |
| `/mode observer` | Switch to observer mode / 옵저버 모드 |
| `/mode participant` | Switch to participant mode (not on KakaoTalk) / 참가자 모드 |

### 단톡방(Group Chat) Assist Workflow / 단톡방 보조 워크플로

KakaoTalk's official Open Builder API does **not** allow bots to join
third-party group chats (`단톡방`). MoA therefore operates in **observer
mode** on KakaoTalk: you forward 단톡방 content into MoA's 1:1 chat,
and MoA replies with a one-tap button to share its answer back to the
원하는 단톡방.

**Inbound — getting 단톡방 content into MoA**

1. In your 단톡방, long-press a message (or multi-select several) →
   tap **Share / 공유** → pick the MoA channel.
2. Optional: paste a `대화 내보내기 .txt` export into the 1:1 chat
   to ingest a whole conversation history.
3. Use `/case start <사건명>` to pin a sticky case session before
   forwarding so all subsequent shares accumulate under the same
   memory namespace. Then "어제 의뢰인이 뭐라 했지?" returns just
   that case's history.

**Outbound — sending MoA's answer back to the 단톡방**

1. MoA's reply appears in your 1:1 chat.
2. Tap the **📤 단톡방으로 보내기** quick-reply button.
3. KakaoTalk's native share picker opens — select the target 단톡방.
4. The message is posted to that 단톡방 with a `[🤖 AI 답변]`
   prefix so all participants see it came from an AI.

Every share-back requires an explicit tap — by design, for legal and
professional safety. Set `javascript_app_key` (Step 4 above) to enable
the one-tap button. Without it, you can still copy/paste manually.

**Taps minimization tip — pin MoA in the OS share sheet**

- **iOS**: Open the share sheet → swipe app row to the end → tap
  **Edit Actions** → pin MoA to the top.
- **Android**: Open the share sheet → long-press the MoA app icon →
  **Pin**. (Requires Android 11+; older versions show MoA in the
  recents row.)

After pinning, sharing 단톡방 messages to MoA is a 2-tap operation:
long-press → tap MoA.

---

## Telegram / 텔레그램

### Step 1: Create Bot / 봇 생성

1. Open Telegram, search for `@BotFather`
2. Send `/newbot` and follow the prompts
3. Copy the bot token (e.g., `123456:ABC-DEF...`)

### Step 2: Configure / 설정

```bash
export ZEROCLAW_TELEGRAM_TOKEN="YOUR_BOT_TOKEN"
export ZEROCLAW_TELEGRAM_ALLOWED_USERS="*"
```

Or in `config.toml`:
```toml
[channels_config.telegram]
token = "YOUR_BOT_TOKEN"
allowed_users = ["*"]
```

### Step 3: Start / 시작

```bash
zeroclaw run --channel telegram
```

Send a message to your bot in Telegram!

---

## Discord / 디스코드

### Step 1: Create Bot / 봇 생성

1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Create New Application → Bot section → Reset Token
3. Copy the bot token
4. Enable **Message Content Intent** under Privileged Gateway Intents
5. Invite bot to your server with the OAuth2 URL Generator (scopes: bot; permissions: Send Messages, Read Messages)

### Step 2: Configure / 설정

```bash
export ZEROCLAW_DISCORD_TOKEN="YOUR_BOT_TOKEN"
export ZEROCLAW_DISCORD_ALLOWED_USERS="*"
```

### Step 3: Start / 시작

```bash
zeroclaw run --channel discord
```

---

## Slack / 슬랙

### Step 1: Create App / 앱 생성

1. Go to [Slack API](https://api.slack.com/apps)
2. Create New App → From scratch
3. Enable Socket Mode
4. Add Bot Token Scopes: `chat:write`, `app_mentions:read`, `im:history`, `im:read`, `im:write`
5. Install to Workspace
6. Copy **Bot Token** (`xoxb-...`) and **App Token** (`xapp-...`)

### Step 2: Configure / 설정

```bash
export ZEROCLAW_SLACK_TOKEN="xoxb-YOUR_BOT_TOKEN"
export ZEROCLAW_SLACK_APP_TOKEN="xapp-YOUR_APP_TOKEN"
export ZEROCLAW_SLACK_ALLOWED_USERS="*"
```

### Step 3: Start / 시작

```bash
zeroclaw run --channel slack
```

---

## LINE / 라인

### Step 1: Create Channel / 채널 생성

1. Go to [LINE Developers](https://developers.line.biz)
2. Create a Provider → Create Messaging API Channel
3. Copy **Channel Access Token** and **Channel Secret**

### Step 2: Set Webhook / 웹훅 설정

Set webhook URL: `https://your-domain:PORT/line/webhook`

### Step 3: Configure / 설정

```bash
export ZEROCLAW_LINE_CHANNEL_ACCESS_TOKEN="YOUR_TOKEN"
export ZEROCLAW_LINE_CHANNEL_SECRET="YOUR_SECRET"
export ZEROCLAW_LINE_ALLOWED_USERS="*"
```

---

## WhatsApp / 왓츠앱

### Step 1: Setup Business Account / 비즈니스 계정

1. Go to [Meta for Developers](https://developers.facebook.com)
2. Create App → Business type → Add WhatsApp product
3. Configure a test phone number
4. Copy **Access Token**, **Phone Number ID**, and **Verify Token**

### Step 2: Configure Webhook / 웹훅 설정

Set webhook URL: `https://your-domain:PORT/whatsapp/webhook`
Set verify token to match your config

### Step 3: Configure / 설정

```bash
export ZEROCLAW_WHATSAPP_ACCESS_TOKEN="YOUR_TOKEN"
export ZEROCLAW_WHATSAPP_PHONE_NUMBER_ID="YOUR_PHONE_ID"
export ZEROCLAW_WHATSAPP_VERIFY_TOKEN="YOUR_VERIFY_TOKEN"
export ZEROCLAW_WHATSAPP_ALLOWED_NUMBERS="*"
```

---

## Exposing Webhooks (for KakaoTalk, LINE, WhatsApp) / 웹훅 외부 노출

Channels that use webhooks need a public URL. Options:

### Option 1: ngrok (Easiest for testing)
```bash
ngrok http 8787
# Use the https://xxx.ngrok.io URL
```

### Option 2: Built-in Tunnel
```bash
# Cloudflare tunnel
zeroclaw gateway --tunnel cloudflare

# Tailscale funnel
zeroclaw gateway --tunnel tailscale
```

### Option 3: Reverse Proxy (Production)
Use nginx, Caddy, or similar with SSL termination.

---

## One-Click Pairing / 원클릭 페어링

For channels that support it (KakaoTalk, Telegram, etc.), new users can self-connect:

1. User sends a message to the bot
2. Bot replies with a "Connect" button/link
3. User clicks → web login page → auto-paired
4. Next message is accepted automatically

No manual allowlist editing required!

수동으로 허용 목록을 편집할 필요 없습니다!

---

## Troubleshooting / 문제 해결

### Webhook not receiving messages / 웹훅이 메시지를 받지 못함
- Check that your webhook URL is publicly accessible
- Verify the port matches your config
- Check firewall rules

### Authentication errors / 인증 오류
- Verify API keys are correct (no extra spaces)
- Check that the bot has proper permissions
- Ensure allowed_users includes the user ID or "*"

### Messages not sending / 메시지 발송 안 됨
- Check the gateway logs: `zeroclaw gateway` (watch terminal output)
- Verify the channel health: `zeroclaw status`
- Test connectivity: `zeroclaw doctor`

---

*For more details, see [Configuration Reference](config-reference.md) and [Troubleshooting Guide](troubleshooting.md).*
