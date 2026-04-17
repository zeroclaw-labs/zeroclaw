<p align="center">
  <img src="https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/docs/assets/zeroclaw-banner.png" alt="ZeroClaw" width="600" />
</p>

<h1 align="center">🦀 ZeroClaw — 개인 AI 어시스턴트</h1>

<p align="center">
  <strong>오버헤드 없음. 타협 없음. 100% Rust. 100% 독립적.</strong><br>
  ⚡️ <strong>$10 하드웨어에서 <5MB RAM으로 실행: OpenClaw보다 99% 적은 메모리, Mac mini보다 98% 저렴!</strong>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/ci-run.yml"><img src="https://img.shields.io/github/actions/workflow/status/zeroclaw-labs/zeroclaw/ci-run.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/releases/latest"><img src="https://img.shields.io/badge/version-v0.6.9-blue" alt="Version v0.6.9" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-Join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Harvard, MIT, 그리고 Sundai.Club 커뮤니티의 학생들과 멤버들이 만들었습니다.
</p>

<p align="center">
  🌐 <strong>언어:</strong>
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

ZeroClaw는 자신의 기기에서 실행하는 개인 AI 어시스턴트입니다. 이미 사용하고 있는 채널(WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work 등)에서 응답합니다. 실시간 제어를 위한 웹 대시보드가 있으며 하드웨어 주변기기(ESP32, STM32, Arduino, Raspberry Pi)에 연결할 수 있습니다. Gateway는 단순한 제어 평면이며, 제품은 어시스턴트 자체입니다.

로컬에서 빠르고 항상 켜져 있는 개인 단일 사용자 어시스턴트를 원한다면 바로 이것입니다.

<p align="center">
  <a href="https://zeroclawlabs.ai">웹사이트</a> ·
  <a href="docs/README.md">문서</a> ·
  <a href="docs/architecture.md">아키텍처</a> ·
  <a href="#빠른-시작-tldr">시작하기</a> ·
  <a href="#openclaw에서-마이그레이션">OpenClaw에서 마이그레이션</a> ·
  <a href="docs/ops/troubleshooting.md">문제 해결</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **권장 설정:** 터미널에서 `zeroclaw onboard`를 실행하세요. ZeroClaw Onboard가 gateway, workspace, 채널, 제공자 설정을 단계별로 안내합니다. macOS, Linux, Windows(WSL2)에서 작동하는 권장 설정 경로입니다. 새로 설치하시나요? 여기서 시작하세요: [시작하기](#빠른-시작-tldr)

### Subscription Auth (OAuth)

- **OpenAI Codex** (ChatGPT 구독)
- **Gemini** (Google OAuth)
- **Anthropic** (API 키 또는 인증 토큰)

모델 참고: 많은 제공자/모델이 지원되지만, 최상의 경험을 위해 사용 가능한 최신 세대의 가장 강력한 모델을 사용하세요. [온보딩](#빠른-시작-tldr)을 참조하세요.

모델 구성 + CLI: [Providers reference](docs/reference/api/providers-reference.md)
인증 프로필 교체(OAuth vs API 키) + 장애 조치: [Model failover](docs/reference/api/providers-reference.md)

## 설치 (권장)

런타임: Rust stable 툴체인. 단일 바이너리, 런타임 의존성 없음.

### Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### 원클릭 부트스트랩

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

`zeroclaw onboard`는 설치 후 자동으로 실행되어 workspace와 제공자를 구성합니다.

## 빠른 시작 (TL;DR)

전체 초보자 가이드(인증, 페어링, 채널): [시작하기](docs/setup-guides/one-click-bootstrap.md)

```bash
# 설치 + 온보드
./install.sh --api-key "sk-..." --provider openrouter

# Gateway 시작 (webhook 서버 + 웹 대시보드)
zeroclaw gateway                # 기본값: 127.0.0.1:42617
zeroclaw gateway --port 0       # 랜덤 포트 (보안 강화)

# 어시스턴트와 대화
zeroclaw agent -m "Hello, ZeroClaw!"

# 대화형 모드
zeroclaw agent

# 완전 자율 런타임 시작 (gateway + 채널 + cron + hands)
zeroclaw daemon

# 상태 확인
zeroclaw status

# 진단 실행
zeroclaw doctor
```

업그레이드 하셨나요? 업데이트 후 `zeroclaw doctor`를 실행하세요.

### 소스에서 빌드 (개발용)

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard
```

> **개발 폴백 (글로벌 설치 없이):** 명령 앞에 `cargo run --release --`를 붙이세요 (예: `cargo run --release -- status`).

## OpenClaw에서 마이그레이션

ZeroClaw는 OpenClaw workspace, 메모리, 구성을 가져올 수 있습니다:

```bash
# 마이그레이션 대상 미리보기 (안전, 읽기 전용)
zeroclaw migrate openclaw --dry-run

# 마이그레이션 실행
zeroclaw migrate openclaw
```

이것은 메모리 항목, workspace 파일, 구성을 `~/.openclaw/`에서 `~/.zeroclaw/`로 마이그레이션합니다. 구성은 JSON에서 TOML로 자동 변환됩니다.

## 보안 기본값 (DM 접근)

ZeroClaw는 실제 메시징 서비스에 연결됩니다. 수신 DM을 신뢰할 수 없는 입력으로 취급하세요.

전체 보안 가이드: [SECURITY.md](SECURITY.md)

모든 채널의 기본 동작:

- **DM 페어링** (기본값): 알 수 없는 발신자는 짧은 페어링 코드를 받으며 봇은 메시지를 처리하지 않습니다.
- 승인: `zeroclaw pairing approve <channel> <code>` (발신자가 로컬 허용 목록에 추가됩니다).
- 공개 수신 DM은 `config.toml`에서 명시적 옵트인이 필요합니다.
- `zeroclaw doctor`를 실행하여 위험하거나 잘못 구성된 DM 정책을 확인하세요.

**자율성 수준:**

| 수준 | 동작 |
|-------|----------|
| `ReadOnly` | 에이전트가 관찰만 할 수 있고 행동하지 않음 |
| `Supervised` (기본값) | 에이전트가 중/고위험 작업에 대해 승인을 받고 행동 |
| `Full` | 에이전트가 정책 범위 내에서 자율적으로 행동 |

**샌드박싱 계층:** workspace 격리, 경로 탐색 차단, 명령 허용 목록, 금지 경로 (`/etc`, `/root`, `~/.ssh`), 속도 제한 (시간당 최대 작업 수, 일일 비용 상한).

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 공지사항

이 표를 사용하여 중요한 공지사항(호환성 변경, 보안 권고, 유지보수 기간, 릴리스 차단)을 확인하세요.

| 날짜 (UTC) | 수준 | 공지 | 조치 |
| ---------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-02-19 | _치명적_ | 우리는 `openagen/zeroclaw`, `zeroclaw.org` 또는 `zeroclaw.net`과 **관련이 없습니다**. `zeroclaw.org`과 `zeroclaw.net` 도메인은 현재 `openagen/zeroclaw` 포크를 가리키고 있으며, 해당 도메인/저장소는 우리의 공식 웹사이트/프로젝트를 사칭하고 있습니다. | 해당 소스의 정보, 바이너리, 모금, 공지를 신뢰하지 마세요. [이 저장소](https://github.com/zeroclaw-labs/zeroclaw)와 검증된 소셜 계정만 사용하세요. |
| 2026-02-19 | _중요_ | Anthropic이 2026-02-19에 인증 및 자격증명 사용 약관을 업데이트했습니다. Claude Code OAuth 토큰(Free, Pro, Max)은 Claude Code와 Claude.ai 전용입니다. 다른 제품, 도구 또는 서비스(Agent SDK 포함)에서 Claude Free/Pro/Max OAuth 토큰을 사용하는 것은 허용되지 않으며 소비자 이용약관을 위반할 수 있습니다. | 잠재적 손실을 방지하기 위해 일시적으로 Claude Code OAuth 통합을 피하세요. 원본 조항: [Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use). |

## 주요 특징

- **기본 경량 런타임** — 일반적인 CLI 및 상태 워크플로우가 릴리스 빌드에서 몇 메가바이트의 메모리 범위 내에서 실행됩니다.
- **비용 효율적인 배포** — $10 보드와 소규모 클라우드 인스턴스를 위해 설계되었으며, 무거운 런타임 의존성이 없습니다.
- **빠른 콜드 스타트** — 단일 바이너리 Rust 런타임으로 명령 및 데몬 시작이 거의 즉각적입니다.
- **이식 가능한 아키텍처** — 교체 가능한 제공자/채널/도구로 ARM, x86, RISC-V에서 하나의 바이너리.
- **로컬 우선 Gateway** — 세션, 채널, 도구, cron, SOP, 이벤트를 위한 단일 제어 평면.
- **멀티 채널 수신함** — WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, Nostr, Mattermost, Nextcloud Talk, DingTalk, Lark, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WebSocket 등.
- **멀티 에이전트 오케스트레이션 (Hands)** — 스케줄에 따라 실행되고 시간이 지남에 따라 더 똑똑해지는 자율 에이전트 스웜.
- **표준 운영 절차 (SOPs)** — MQTT, webhook, cron, 주변기기 트리거를 통한 이벤트 기반 워크플로우 자동화.
- **웹 대시보드** — 실시간 채팅, 메모리 브라우저, 구성 편집기, cron 관리자, 도구 검사기를 갖춘 React 19 + Vite 웹 UI.
- **하드웨어 주변기기** — `Peripheral` 트레이트를 통한 ESP32, STM32 Nucleo, Arduino, Raspberry Pi GPIO.
- **일급 도구** — shell, file I/O, browser, git, web fetch/search, MCP, Jira, Notion, Google Workspace 등 70개 이상.
- **라이프사이클 훅** — 모든 단계에서 LLM 호출, 도구 실행, 메시지를 가로채고 수정.
- **스킬 플랫폼** — 번들, 커뮤니티, workspace 스킬과 보안 감사.
- **터널 지원** — 원격 접속을 위한 Cloudflare, Tailscale, ngrok, OpenVPN, 사용자 정의 터널.

### 팀이 ZeroClaw를 선택하는 이유

- **기본 경량:** 작은 Rust 바이너리, 빠른 시작, 낮은 메모리 사용.
- **기본 보안:** 페어링, 엄격한 샌드박싱, 명시적 허용 목록, workspace 범위 지정.
- **완전히 교체 가능:** 핵심 시스템이 트레이트(제공자, 채널, 도구, 메모리, 터널).
- **벤더 락인 없음:** OpenAI 호환 제공자 지원 + 플러그 가능한 사용자 정의 엔드포인트.

## 벤치마크 스냅샷 (ZeroClaw vs OpenClaw, 재현 가능)

로컬 머신 빠른 벤치마크 (macOS arm64, 2026년 2월) 0.8GHz 엣지 하드웨어로 정규화.

|                           | OpenClaw      | NanoBot        | PicoClaw        | ZeroClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **언어**              | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **시작 (0.8GHz 코어)** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **바이너리 크기**           | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **비용**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **모든 하드웨어 $10** |

> 참고: ZeroClaw 결과는 `/usr/bin/time -l`을 사용한 릴리스 빌드에서 측정되었습니다. OpenClaw는 Node.js 런타임이 필요하며(일반적으로 ~390MB 추가 메모리 오버헤드), NanoBot은 Python 런타임이 필요합니다. PicoClaw와 ZeroClaw는 정적 바이너리입니다. 위 RAM 수치는 런타임 메모리이며, 빌드 시 컴파일 요구사항은 더 높습니다.

<p align="center">
  <img src="docs/assets/zeroclaw-comparison.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

### 재현 가능한 로컬 측정

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

## 지금까지 구축한 모든 것

### 핵심 플랫폼

- 세션, 프레즌스, 구성, cron, webhook, 웹 대시보드, 페어링을 갖춘 Gateway HTTP/WS/SSE 제어 평면.
- CLI 표면: `gateway`, `agent`, `onboard`, `doctor`, `status`, `service`, `migrate`, `auth`, `cron`, `channel`, `skills`.
- 도구 디스패치, 프롬프트 구성, 메시지 분류, 메모리 로딩을 갖춘 에이전트 오케스트레이션 루프.
- 보안 정책 적용, 자율성 수준, 승인 게이팅을 갖춘 세션 모델.
- 20개 이상의 LLM 백엔드에 걸쳐 장애 조치, 재시도, 모델 라우팅을 갖춘 탄력적 제공자 래퍼.

### 채널

채널: WhatsApp (네이티브), Telegram, Slack, Discord, Signal, iMessage, Matrix, IRC, Email, Bluesky, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, QQ, Reddit, LinkedIn, Twitter, MQTT, WeChat Work, WATI, Mochat, Linq, Notion, WebSocket, ClawdTalk.

기능 게이트: Matrix (`channel-matrix`), Lark (`channel-lark`), Nostr (`channel-nostr`).

### 웹 대시보드

Gateway에서 직접 제공하는 React 19 + Vite 6 + Tailwind CSS 4 웹 대시보드:

- **대시보드** — 시스템 개요, 상태, 가동 시간, 비용 추적
- **에이전트 채팅** — 에이전트와의 대화형 채팅
- **메모리** — 메모리 항목 탐색 및 관리
- **구성** — 구성 보기 및 편집
- **Cron** — 예약된 작업 관리
- **도구** — 사용 가능한 도구 탐색
- **로그** — 에이전트 활동 로그 보기
- **비용** — 토큰 사용량 및 비용 추적
- **Doctor** — 시스템 상태 진단
- **통합** — 통합 상태 및 설정
- **페어링** — 기기 페어링 관리

### 펌웨어 대상

| 대상 | 플랫폼 | 용도 |
|--------|----------|---------|
| ESP32 | Espressif ESP32 | 무선 주변기기 에이전트 |
| ESP32-UI | ESP32 + Display | 시각적 인터페이스를 갖춘 에이전트 |
| STM32 Nucleo | STM32 (ARM Cortex-M) | 산업용 주변기기 |
| Arduino | Arduino | 기본 센서/액추에이터 브릿지 |
| Uno Q Bridge | Arduino Uno | 에이전트와의 시리얼 브릿지 |

### 도구 + 자동화

- **코어:** shell, file read/write/edit, git operations, glob search, content search
- **웹:** browser control, web fetch, web search, screenshot, image info, PDF read
- **통합:** Jira, Notion, Google Workspace, Microsoft 365, LinkedIn, Composio, Pushover
- **MCP:** Model Context Protocol tool wrapper + deferred tool sets
- **스케줄링:** cron add/remove/update/run, schedule tool
- **메모리:** recall, store, forget, knowledge, project intel
- **고급:** delegate (agent-to-agent), swarm, model switch/routing, security ops, cloud ops
- **하드웨어:** board info, memory map, memory read (feature-gated)

### 런타임 + 안전

- **자율성 수준:** ReadOnly, Supervised (기본값), Full.
- **샌드박싱:** workspace 격리, 경로 탐색 차단, 명령 허용 목록, 금지 경로, Landlock (Linux), Bubblewrap.
- **속도 제한:** 시간당 최대 작업 수, 일일 최대 비용 (구성 가능).
- **승인 게이팅:** 중/고위험 작업에 대한 대화형 승인.
- **긴급 정지:** 긴급 종료 기능.
- **129개 이상의 보안 테스트** 자동화된 CI에서.

### 운영 + 패키징

- Gateway에서 직접 제공하는 웹 대시보드.
- 터널 지원: Cloudflare, Tailscale, ngrok, OpenVPN, custom command.
- 컨테이너화된 실행을 위한 Docker 런타임 어댑터.
- CI/CD: beta (push 시 자동) → stable (수동 디스패치) → Docker, crates.io, Scoop, AUR, Homebrew, tweet.
- Linux (x86_64, aarch64, armv7), macOS (x86_64, aarch64), Windows (x86_64)용 사전 빌드 바이너리.


## 구성

최소 `~/.zeroclaw/config.toml`:

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

전체 구성 참조: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md).

### 채널 구성

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

### 터널 구성

```toml
[tunnel]
kind = "cloudflare"  # 또는 "tailscale", "ngrok", "openvpn", "custom", "none"
```

상세 정보: [Channel reference](docs/reference/api/channels-reference.md) · [Config reference](docs/reference/api/config-reference.md)

### 현재 런타임 지원

- **`native`** (기본값) — 직접 프로세스 실행, 가장 빠른 경로, 신뢰할 수 있는 환경에 적합.
- **`docker`** — 완전한 컨테이너 격리, 강화된 보안 정책, Docker 필요.

엄격한 샌드박싱이나 네트워크 격리를 위해 `runtime.kind = "docker"`를 설정하세요.

## Subscription Auth (OpenAI Codex / Claude Code / Gemini)

ZeroClaw는 구독 기반 인증 프로필(다중 계정, 저장 시 암호화)을 지원합니다.

- 저장 파일: `~/.zeroclaw/auth-profiles.json`
- 암호화 키: `~/.zeroclaw/.secret_key`
- 프로필 id 형식: `<provider>:<profile_name>` (예: `openai-codex:work`)

```bash
# OpenAI Codex OAuth (ChatGPT 구독)
zeroclaw auth login --provider openai-codex --device-code

# Gemini OAuth
zeroclaw auth login --provider gemini --profile default

# Anthropic setup-token
zeroclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# 확인 / 갱신 / 프로필 전환
zeroclaw auth status
zeroclaw auth refresh --provider openai-codex --profile default
zeroclaw auth use --provider openai-codex --profile work

# 구독 인증으로 에이전트 실행
zeroclaw agent --provider openai-codex -m "hello"
zeroclaw agent --provider anthropic -m "hello"
```

## 에이전트 workspace + 스킬

Workspace 루트: `~/.zeroclaw/workspace/` (구성을 통해 변경 가능).

주입되는 프롬프트 파일:
- `IDENTITY.md` — 에이전트 성격과 역할
- `USER.md` — 사용자 컨텍스트와 선호도
- `MEMORY.md` — 장기 사실과 교훈
- `AGENTS.md` — 세션 규칙과 초기화 규칙
- `SOUL.md` — 핵심 정체성과 운영 원칙

스킬: `~/.zeroclaw/workspace/skills/<skill>/SKILL.md` 또는 `SKILL.toml`.

```bash
# 설치된 스킬 목록
zeroclaw skills list

# git에서 설치
zeroclaw skills install https://github.com/user/my-skill.git

# 설치 전 보안 감사
zeroclaw skills audit https://github.com/user/my-skill.git

# 스킬 제거
zeroclaw skills remove my-skill
```

## CLI 명령어

```bash
# Workspace 관리
zeroclaw onboard              # 안내된 설정 마법사
zeroclaw status               # 데몬/에이전트 상태 표시
zeroclaw doctor               # 시스템 진단 실행

# Gateway + 데몬
zeroclaw gateway              # Gateway 서버 시작 (127.0.0.1:42617)
zeroclaw daemon               # 완전 자율 런타임 시작

# 에이전트
zeroclaw agent                # 대화형 채팅 모드
zeroclaw agent -m "message"   # 단일 메시지 모드

# 서비스 관리
zeroclaw service install      # OS 서비스로 설치 (launchd/systemd)
zeroclaw service start|stop|restart|status

# 채널
zeroclaw channel list         # 구성된 채널 목록
zeroclaw channel doctor       # 채널 상태 확인
zeroclaw channel bind-telegram 123456789

# Cron + 스케줄링
zeroclaw cron list            # 예약된 작업 목록
zeroclaw cron add "*/5 * * * *" --prompt "Check system health"
zeroclaw cron remove <id>

# 메모리
zeroclaw memory list          # 메모리 항목 목록
zeroclaw memory get <key>     # 메모리 조회
zeroclaw memory stats         # 메모리 통계

# 인증 프로필
zeroclaw auth login --provider <name>
zeroclaw auth status
zeroclaw auth use --provider <name> --profile <profile>

# 하드웨어 주변기기
zeroclaw hardware discover    # 연결된 기기 스캔
zeroclaw peripheral list      # 연결된 주변기기 목록
zeroclaw peripheral flash     # 기기에 펌웨어 플래시

# 마이그레이션
zeroclaw migrate openclaw --dry-run
zeroclaw migrate openclaw

# 셸 자동완성
source <(zeroclaw completions bash)
zeroclaw completions zsh > ~/.zfunc/_zeroclaw
```

전체 명령어 참조: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## 사전 요구사항

<details>
<summary><strong>Windows</strong></summary>

#### 필수

1. **Visual Studio Build Tools** (MSVC 링커와 Windows SDK 제공):

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    설치 중(또는 Visual Studio Installer를 통해) **"C++를 사용한 데스크톱 개발"** 워크로드를 선택하세요.

2. **Rust 툴체인:**

    ```powershell
    winget install Rustlang.Rustup
    ```

    설치 후 새 터미널을 열고 `rustup default stable`을 실행하여 stable 툴체인이 활성화되었는지 확인하세요.

3. **확인:** 둘 다 작동하는지 확인:
    ```powershell
    rustc --version
    cargo --version
    ```

#### 선택사항

- **Docker Desktop** — [Docker 샌드박스 런타임](#현재-런타임-지원)을 사용하는 경우에만 필요 (`runtime.kind = "docker"`). `winget install Docker.DockerDesktop`으로 설치.

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### 필수

1. **빌드 필수 도구:**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** Xcode Command Line Tools 설치: `xcode-select --install`

2. **Rust 툴체인:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    자세한 내용은 [rustup.rs](https://rustup.rs)를 참조하세요.

3. **확인:** 둘 다 작동하는지 확인:
    ```bash
    rustc --version
    cargo --version
    ```

#### 한 줄 설치

위 단계를 건너뛰고 모든 것(시스템 의존성, Rust, ZeroClaw)을 한 번에 설치:

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

#### 컴파일 리소스 요구사항

소스에서 빌드하려면 결과 바이너리를 실행하는 것보다 더 많은 리소스가 필요합니다:

| 리소스 | 최소 | 권장 |
| -------------- | ------- | ----------- |
| **RAM + swap** | 2 GB    | 4 GB+       |
| **여유 디스크**  | 6 GB    | 10 GB+      |

호스트가 최소 사양 미만인 경우 사전 빌드 바이너리를 사용하세요:

```bash
./install.sh --prefer-prebuilt
```

소스 빌드 폴백 없이 바이너리만 설치:

```bash
./install.sh --prebuilt-only
```

#### 선택사항

- **Docker** — [Docker 샌드박스 런타임](#현재-런타임-지원)을 사용하는 경우에만 필요 (`runtime.kind = "docker"`). 패키지 관리자 또는 [docker.com](https://docs.docker.com/engine/install/)을 통해 설치.

> **참고:** 기본 `cargo build --release`는 `codegen-units=1`을 사용하여 피크 컴파일 압력을 낮춥니다. 성능이 좋은 머신에서 더 빠른 빌드를 위해 `cargo build --profile release-fast`를 사용하세요.

</details>

<!-- markdownlint-enable MD001 MD024 -->

### 사전 빌드 바이너리

릴리스 에셋은 다음 플랫폼에 게시됩니다:

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64`
- Windows: `x86_64`

최신 에셋 다운로드:
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>

## 문서

온보딩을 마친 후 더 깊은 참조가 필요할 때 사용하세요.

- [문서 인덱스](docs/README.md)에서 탐색과 "무엇이 어디에 있는지"를 확인하세요.
- [아키텍처 개요](docs/architecture.md)에서 전체 시스템 모델을 확인하세요.
- [구성 참조](docs/reference/api/config-reference.md)에서 모든 키와 예제를 확인하세요.
- [운영 런북](docs/ops/operations-runbook.md)으로 Gateway를 운영하세요.
- [ZeroClaw Onboard](#빠른-시작-tldr)를 따라 안내된 설정을 진행하세요.
- [문제 해결 가이드](docs/ops/troubleshooting.md)로 일반적인 오류를 디버그하세요.
- 노출하기 전에 [보안 가이드](docs/security/README.md)를 검토하세요.

### 참조 문서

- 문서 허브: [docs/README.md](docs/README.md)
- 통합 문서 목차: [docs/SUMMARY.md](docs/SUMMARY.md)
- 명령어 참조: [docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- 구성 참조: [docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- 제공자 참조: [docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- 채널 참조: [docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- 운영 런북: [docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- 문제 해결: [docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### 협업 문서

- 기여 가이드: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR 워크플로 정책: [docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI 워크플로 가이드: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 리뷰어 플레이북: [docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- 보안 공개 정책: [SECURITY.md](SECURITY.md)
- 문서 템플릿: [docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### 배포 + 운영

- 네트워크 배포 가이드: [docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- 프록시 에이전트 플레이북: [docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- 하드웨어 가이드: [docs/hardware/README.md](docs/hardware/README.md)

## Icy Crab 🦀

ZeroClaw는 빠르고 효율적인 AI 어시스턴트인 smooth crab 🦀을 위해 만들어졌습니다. Argenis De La Rosa와 커뮤니티가 만들었습니다.

- [zeroclawlabs.ai](https://zeroclawlabs.ai)
- [@zeroclawlabs](https://x.com/zeroclawlabs)

## ZeroClaw 지원하기

### 🙏 특별 감사

이 오픈소스 작업에 영감을 주고 힘을 실어주는 커뮤니티와 기관에 진심으로 감사드립니다:

- **Harvard University** — 지적 호기심을 키우고 가능성의 한계를 넓혀 주셔서.
- **MIT** — 열린 지식, 오픈소스, 그리고 기술이 모두에게 접근 가능해야 한다는 신념을 옹호해 주셔서.
- **Sundai Club** — 커뮤니티, 에너지, 그리고 의미 있는 것을 만들고자 하는 끊임없는 열정.
- **세계 그리고 그 너머** 🌍✨ — 오픈소스를 선한 힘으로 만드는 모든 기여자, 꿈꾸는 이, 그리고 빌더에게. 이것은 여러분을 위한 것입니다.

우리는 최고의 아이디어가 모든 곳에서 나오기 때문에 오픈소스로 구축합니다. 이것을 읽고 있다면 여러분도 그 일부입니다. 환영합니다. 🦀❤️

## 기여하기

ZeroClaw가 처음이신가요? [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) 레이블이 붙은 이슈를 찾아보세요 — 시작하는 방법은 [기여 가이드](CONTRIBUTING.md#first-time-contributors)를 참조하세요. AI/vibe-coded PR도 환영합니다! 🤖

[CONTRIBUTING.md](CONTRIBUTING.md)와 [CLA.md](docs/contributing/cla.md)를 참조하세요. 트레이트를 구현하고 PR을 제출하세요:

- CI 워크플로 가이드: [docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 새 `Provider` → `src/providers/`
- 새 `Channel` → `src/channels/`
- 새 `Observer` → `src/observability/`
- 새 `Tool` → `src/tools/`
- 새 `Memory` → `src/memory/`
- 새 `Tunnel` → `src/tunnel/`
- 새 `Peripheral` → `src/peripherals/`
- 새 `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ 공식 저장소 및 사칭 경고

**이것이 유일한 공식 ZeroClaw 저장소입니다:**

> https://github.com/zeroclaw-labs/zeroclaw

"ZeroClaw"라고 주장하거나 ZeroClaw Labs와의 제휴를 암시하는 다른 저장소, 조직, 도메인 또는 패키지는 **승인되지 않았으며 이 프로젝트와 관련이 없습니다**. 알려진 비인가 포크는 [TRADEMARK.md](docs/maintainers/trademark.md)에 나열됩니다.

사칭이나 상표 오용을 발견하면 [이슈를 열어](https://github.com/zeroclaw-labs/zeroclaw/issues) 신고해 주세요.

---

## 라이선스

ZeroClaw는 최대한의 개방성과 기여자 보호를 위해 듀얼 라이선스가 적용됩니다:

| 라이선스 | 사용 사례 |
|---|---|
| [MIT](LICENSE-MIT) | 오픈소스, 연구, 학술, 개인 사용 |
| [Apache 2.0](LICENSE-APACHE) | 특허 보호, 기관, 상업 배포 |

두 라이선스 중 하나를 선택할 수 있습니다. **기여자는 자동으로 두 가지 모두에 대한 권한을 부여합니다** — 전체 기여자 계약은 [CLA.md](docs/contributing/cla.md)를 참조하세요.

### 상표

**ZeroClaw** 이름과 로고는 ZeroClaw Labs의 상표입니다. 이 라이선스는 승인이나 제휴를 암시하기 위해 사용할 권한을 부여하지 않습니다. 허용 및 금지 사용은 [TRADEMARK.md](docs/maintainers/trademark.md)를 참조하세요.

### 기여자 보호

- 기여의 **저작권을 유지**합니다
- **특허 부여** (Apache 2.0)가 다른 기여자의 특허 청구로부터 보호합니다
- 기여는 커밋 기록과 [NOTICE](NOTICE)에 **영구적으로 귀속**됩니다
- 기여함으로써 상표권이 이전되지 않습니다

---

**ZeroClaw** — 오버헤드 없음. 타협 없음. 어디서나 배포. 무엇이든 교체. 🦀

## 기여자

<a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
</a>

이 목록은 GitHub 기여자 그래프에서 생성되며 자동으로 업데이트됩니다.

## 스타 히스토리

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
