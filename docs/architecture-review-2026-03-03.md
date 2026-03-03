# MoA (ZeroClaw) Architecture Review — 2026-03-03

## Executive Summary

MoA(Master of AI)는 ZeroClaw 엔진 위에 구축된 **Rust-first 자율 에이전트 런타임**으로, 고성능, 고보안, 고확장성을 목표로 설계되었습니다. 본 리뷰는 전체 아키텍처, 주요 모듈(Gateway, Auth, Voice, Channels), 빌드 시스템, 검증 결과를 포함합니다.

---

## 1. 전체 아키텍처 개요

### 1.1 프로젝트 통계

| 항목 | 수치 |
|------|------|
| **총 Rust 코드** | ~261,000 라인, 353개 파일 |
| **채널 수** | 30+ 메시징 플랫폼 |
| **LLM 프로바이더** | 15+ (Anthropic, OpenAI, Gemini, Ollama 등) |
| **도구(Tools)** | 60+ (파일, 브라우저, 셸, 메모리, 크론 등) |
| **메모리 백엔드** | 5+ (SQLite, PostgreSQL, Qdrant, Markdown) |
| **CI/CD 워크플로우** | 33개 |
| **지원 언어** | 7개 (EN, ZH-CN, JA, RU, FR, VI, EL) |
| **빌드 타겟** | 데스크톱, 모바일(iOS/Android), 웹, Docker, ESP32, STM32 |

### 1.2 핵심 모듈 구조

```
src/
├── agent/          # 에이전트 오케스트레이션 루프
├── auth/           # OAuth, 토큰 관리 (Anthropic, OpenAI, Gemini)
├── billing/        # 결제 및 과금
├── channels/       # 30+ 메시징 채널 (Telegram, Discord, KakaoTalk 등)
├── coding/         # 코드 생성 및 리뷰 파이프라인
├── config/         # 스키마, 로딩, 검증
├── gateway/        # Axum HTTP/WebSocket API 서버
├── memory/         # 영구/벡터 메모리 백엔드
├── providers/      # LLM 프로바이더 추상화
├── security/       # 보안 정책, 페어링, 시크릿 스토어, 샌드박싱
├── tools/          # 60+ 도구 실행 표면
├── voice/          # 실시간 음성 해석 및 대화
└── main.rs         # CLI 진입점 및 커맨드 라우팅
```

### 1.3 아키텍처 패턴

- **Trait-Driven Architecture**: `Provider`, `Channel`, `Tool`, `Memory`, `Observer` 등 핵심 트레이트 기반
- **Factory Registration**: 런타임 설정 기반 인스턴스 생성
- **Defense-in-Depth**: 다중 보안 레이어 (페어링 + 웹훅 시크릿 + 채널별 시그니처)
- **Secure by Default**: Localhost 전용 바인딩, deny-by-default 접근 제어

---

## 2. Gateway 모듈 리뷰

### 2.1 구조 (src/gateway/)

| 파일 | 역할 | LOC |
|------|------|-----|
| `mod.rs` | HTTP 서버, 라우팅, 웹훅 핸들러 | ~4,000 |
| `api.rs` | 대시보드 REST API | ~800 |
| `ws.rs` | WebSocket 채팅 핸들러 | ~1,300 |
| `pair.rs` | 채널 원클릭 페어링 | ~400 |
| `openai_compat.rs` | OpenAI 호환 API | ~600 |
| `sse.rs` | Server-Sent Events | ~200 |
| `static_files.rs` | 웹 대시보드 정적 파일 | ~200 |

### 2.2 보안 기능

- **Request Body Limit**: 64KB (메모리 고갈 방지)
- **Request Timeout**: 30초 (slow-loris 공격 방지)
- **Rate Limiting**: 슬라이딩 윈도우, 최대 10,000 클라이언트 추적
- **멱등성 키**: `X-Idempotency-Key` 헤더 지원
- **공개 바인딩 방지**: 터널 없이 공개 IP 바인딩 시 즉시 실패

### 2.3 WebSocket 프로토콜

```
Client → Server: {"type":"message","content":"안녕하세요"}
Server → Client: {"type":"chunk","content":"안녕하세요! "}
Server → Client: {"type":"tool_call","name":"shell","args":{...}}
Server → Client: {"type":"tool_result","name":"shell","output":"..."}
Server → Client: {"type":"done","full_response":"..."}
```

---

## 3. Auth/Security 모듈 리뷰

### 3.1 인증 계층

| 레이어 | 메커니즘 | 상세 |
|--------|---------|------|
| **1. 디바이스 페어링** | Bearer 토큰 + 일회성 코드 | 상수 시간 비교, SHA-256 해시, 브루트포스 잠금 |
| **2. 웹훅 시크릿** | SHA-256 해시 검증 | 채널별 독립 시크릿 |
| **3. 채널별 시그니처** | HMAC-SHA256 | WhatsApp, GitHub, KakaoTalk 등 |
| **4. OAuth 프로필** | OpenAI, Gemini, Anthropic | 자동 토큰 새로고침 |

### 3.2 암호화

- **ChaCha20-Poly1305**: 시크릿 스토어 (인증된 암호화)
- **AES-256-GCM**: 필드 암호화, 백업 볼트
- **PBKDF2-HMAC-SHA256**: 디바이스 바인딩 (100,000회 반복)

### 3.3 보안 정책 (SecurityPolicy)

```
자율성 수준:
  ReadOnly    → 관찰만 가능
  Supervised  → 위험한 작업은 승인 필요
  Full        → 정책 범위 내 완전 자율
```

- **명령어 허용 목록**: git, npm, cargo, ls, cat, grep 등
- **위험 명령 차단**: 백그라운드 체이닝, 셸 리다이렉션, 파이핑
- **워크스페이스 격리**: 작업 디렉토리 외부 접근 차단
- **샌드박싱**: Docker, Firejail, Bubblewrap, Landlock (자동 선택)

### 3.4 개선 권장사항

| 우선순위 | 항목 | 설명 |
|---------|------|------|
| **높음** | 토큰 TTL | Bearer 토큰에 만료 시간 추가 |
| **높음** | 웹훅 시크릿 암호화 | 설정 파일 내 모든 시크릿 암호화 |
| **중간** | Async 뮤텍스 전환 | PairingGuard에서 parking_lot → tokio::sync::Mutex |
| **중간** | Rate Limiter 강화 | 적응형 임계값 또는 리버스 프록시 통합 |
| **낮음** | OAuth2/OIDC 통합 | 엔터프라이즈 배포 지원 |

---

## 4. Voice 모듈 리뷰

### 4.1 구조 (src/voice/)

| 파일 | 역할 |
|------|------|
| `pipeline.rs` | 음성 프로바이더 트레이트, 25개 언어 지원, 세션 관리 |
| `gemini_live.rs` | Gemini Live WebSocket (BidiGenerateContent) |
| `openai_realtime.rs` | OpenAI Realtime API |
| `simul.rs` | 동시통역 세그먼테이션 엔진 |
| `simul_session.rs` | 세션 매니저 (Live API + 세그먼테이션 + 이벤트) |
| `conference.rs` | 다중 참여자 음성 회의 |
| `events.rs` | WebSocket 이벤트 스키마 |

### 4.2 지원 언어 (25개)

한국어, 일본어, 중국어(간/번체), 태국어, 베트남어, 인도네시아어, 말레이어, 필리핀어, 힌디어, 영어, 스페인어, 프랑스어, 독일어, 이탈리아어, 포르투갈어, 네덜란드어, 폴란드어, 체코어, 스웨덴어, 덴마크어, 러시아어, 우크라이나어, 터키어, 아랍어

### 4.3 채널별 음성 지원

| 플랫폼 | 음성 수신 | 음성 발신 | 최대 길이 | 포맷 |
|--------|----------|----------|----------|------|
| **Telegram** | O | O | 무제한 | OGG, Opus, MP3, M4A, WAV |
| **Discord** | O | O | 10분 | OGG, MP3, M4A, WAV, WebM |
| **KakaoTalk** | O | X | 5분 | M4A, MP3, AAC |
| **WhatsApp Web** | O | ? | - | OGG, MP3 등 |

---

## 5. Channels 모듈 리뷰 (KakaoTalk 집중)

### 5.1 KakaoTalk 채널 구현 상세

**파일**: `src/channels/kakao.rs` (971 라인)

#### 아키텍처
- **수신**: Axum HTTP 웹훅 서버 (`POST /kakao/webhook`)
- **발신**: Kakao REST API (Admin Key 인증)
- **두 가지 웹훅 포맷 지원**:
  1. Chatbot Skill API (권장): `userRequest` → JSON 응답
  2. Direct Callback: `content` → StatusCode 응답

#### 설정 (`config.toml`)
```toml
[channels_config.kakao]
rest_api_key = "your_kakao_rest_api_key"
admin_key = "your_kakao_admin_key"
webhook_secret = "optional_hmac_secret"
allowed_users = ["*"]  # "*" = 모든 사용자 허용
port = 8787
```

#### 환경 변수
```bash
ZEROCLAW_KAKAO_REST_API_KEY=your_key
ZEROCLAW_KAKAO_ADMIN_KEY=your_admin_key
ZEROCLAW_KAKAO_ALLOWED_USERS=*
```

#### 기능
- **메시지 분할**: 1000자 제한 (문자 수 기준, 바이트가 아님)
- **리치 메시지**: 캐러셀, 버튼, 알림톡 템플릿
- **원격 명령**: `/status`, `/memory`, `/remember`, `/forget`, `/cron`, `/help`, `/shell`
- **원클릭 페어링**: 웹 기반 자동 연결 흐름
- **HMAC-SHA256 검증**: 웹훅 시그니처 (선택)
- **음성 수신**: M4A, MP3, AAC (발신 미지원)

#### 테스트 커버리지
- 사용자 허용 목록 (와일드카드, 특정 ID, 비어있음)
- 메시지 분할 (UTF-8 안전, 한국어 문자 처리)
- 원격 명령 파싱
- 캐러셀/버튼 템플릿 구조
- 웹훅 시그니처 검증
- 설정 직렬화/역직렬화

### 5.2 전체 채널 목록

**기본 포함 (30+)**:
Telegram, Discord, Slack, WhatsApp Cloud, WhatsApp Web, Signal, iMessage, IRC, Email, QQ, Napcat, DingTalk, **KakaoTalk**, LINE, GitHub, Nextcloud Talk, Mattermost, WATI, Linq, Nostr, ClawdTalk, ACP, BlueBubbles, CLI, MQTT

**Feature Flag 필요**:
- Matrix (`channel-matrix`)
- Lark/Feishu (`channel-lark`)
- WhatsApp Web Storage (`whatsapp-web`)

---

## 6. 검증 결과

### 6.1 cargo fmt

```
상태: 포맷 차이 발견 (4개 파일)
- src/auth/email_verify.rs: 줄바꿈 정리 필요
- src/channels/voice_features.rs: 트레이싱 매크로 정리
- src/coding/auto_fix.rs: 사소한 포맷
```

**조치**: `cargo fmt --all` 실행으로 자동 수정 가능

### 6.2 cargo clippy

```
상태: 통과 (메인 라이브러리 코드)
참고: wiremock 0.6.5 dev-dependency가 Rust 1.87에서 호환성 문제
      (let chain 기능은 Rust 1.88+에서 안정화)
```

### 6.3 cargo test

```
상태: 컴파일 실패 (wiremock 0.6.5 호환성)
원인: wiremock 0.6.5가 불안정한 'let' 표현식 사용
해결: wiremock 버전을 0.6.4로 다운그레이드하거나 Rust 1.88+로 업그레이드
```

**권장 조치**:
1. `Cargo.toml`에서 `wiremock = "0.6.4"` 로 고정, 또는
2. `rust-toolchain.toml`을 1.88+로 업데이트

---

## 7. 앱 빌드 방법

### 7.1 필요 조건

- **RAM**: 2GB+ (4GB 권장)
- **디스크**: 6GB+
- **Rust**: 1.87+ (`rustup install 1.87`)
- **OS**: Linux, macOS, Windows

### 7.2 백엔드 (Rust 바이너리) 빌드

```bash
# 소스 빌드 (가장 기본)
git clone https://github.com/Kimjaechol/MoA_new.git
cd MoA_new
cargo build --release --locked
# 바이너리: target/release/zeroclaw (~5-10MB)

# 설치
cargo install --path . --locked

# 빠른 빌드 (16GB+ RAM)
cargo build --profile release-fast --locked

# 특정 기능 포함
cargo build --release --locked --features hardware,channel-matrix
```

### 7.3 웹 대시보드 빌드

```bash
cd web/
npm install
npm run build
# 빌드 결과: web/dist/ → 바이너리에 임베드됨
```

### 7.4 Tauri 데스크톱 앱 빌드

```bash
cd clients/tauri/
npm install
npm run tauri:build
# macOS: .dmg, Windows: .msi, Linux: .AppImage
```

### 7.5 모바일 빌드

```bash
# Android
cd clients/tauri/
npm run mobile:setup:android
npm run tauri:android:build

# iOS
npm run mobile:setup:ios
npm run tauri:ios:build
```

### 7.6 Docker 빌드

```bash
# Dev 이미지
docker build --target dev -t moa:dev .

# Production 이미지 (Distroless)
docker build --target release -t moa:release .

# Docker Compose
docker compose up -d
```

### 7.7 원클릭 설치

```bash
# 인터랙티브 가이드 모드
./bootstrap.sh --guided

# 시스템 의존성 + Rust 설치 포함
./bootstrap.sh --install-system-deps --install-rust

# 사전 빌드된 바이너리 우선
./bootstrap.sh --prefer-prebuilt
```

---

## 8. 채널 세팅 가이드

### 8.1 KakaoTalk 채널 세팅 (테스트 우선순위 1위)

#### 단계 1: Kakao Developers 앱 등록

1. https://developers.kakao.com 접속
2. "애플리케이션 추가" 클릭
3. 앱 이름: "MoA" (또는 원하는 이름)
4. 사업자 정보 입력 (비즈니스 채널 필요)

#### 단계 2: API 키 확인

1. 앱 설정 → "앱 키" 메뉴
2. **REST API 키** 복사 → `rest_api_key`
3. **Admin 키** 복사 → `admin_key`

#### 단계 3: 카카오톡 채널 연결

1. 카카오톡 채널 관리자 센터 (https://center-pf.kakao.com)
2. 채널 생성 또는 기존 채널 선택
3. Developers 앱에 채널 연결

#### 단계 4: Chatbot 설정

1. Kakao i 오픈빌더 (https://chatbot.kakao.com)
2. 봇 생성 → 스킬 서버 등록
3. 스킬 URL: `https://your-domain:8787/kakao/webhook`
4. 시나리오에 스킬 연결

#### 단계 5: MoA 설정

**방법 A: 설정 파일** (`~/.zeroclaw/config.toml`)
```toml
[channels_config.kakao]
rest_api_key = "YOUR_REST_API_KEY"
admin_key = "YOUR_ADMIN_KEY"
webhook_secret = "OPTIONAL_SECRET"
allowed_users = ["*"]
port = 8787
```

**방법 B: 환경 변수**
```bash
export ZEROCLAW_KAKAO_REST_API_KEY="YOUR_REST_API_KEY"
export ZEROCLAW_KAKAO_ADMIN_KEY="YOUR_ADMIN_KEY"
export ZEROCLAW_KAKAO_ALLOWED_USERS="*"
```

#### 단계 6: 실행 및 테스트

```bash
# Gateway 시작
zeroclaw gateway

# 카카오톡에서 채널 친구 추가 후 메시지 발송
# 응답이 돌아오면 성공!
```

### 8.2 웹 채팅 세팅

```bash
# 1. 웹 대시보드 빌드
cd web/ && npm install && npm run build && cd ..

# 2. Gateway 실행
zeroclaw gateway
# → http://localhost:42617 접속

# 3. 페어링 코드로 인증
# 터미널에 표시되는 페어링 코드 입력
```

### 8.3 Telegram 세팅

```bash
# 1. BotFather에서 봇 토큰 받기
# 2. 설정
export ZEROCLAW_TELEGRAM_TOKEN="123456:ABC..."
export ZEROCLAW_TELEGRAM_ALLOWED_USERS="*"

# 3. 실행
zeroclaw run --channel telegram
```

### 8.4 Discord 세팅

```bash
# 1. Discord Developer Portal에서 봇 토큰 받기
# 2. 설정
export ZEROCLAW_DISCORD_TOKEN="your_bot_token"
export ZEROCLAW_DISCORD_ALLOWED_USERS="*"

# 3. 실행
zeroclaw run --channel discord
```

---

## 9. 테스트 계획

### 9.1 1차 테스트: 앱 기본 동작

```bash
# CLI 모드로 기본 대화 테스트
zeroclaw chat

# 건강 점검
zeroclaw doctor
zeroclaw status
```

### 9.2 2차 테스트: 웹 채팅 + 채널 연결

```bash
# Step 1: Gateway 시작
zeroclaw gateway

# Step 2: 브라우저에서 웹 채팅 접속
open http://localhost:42617

# Step 3: KakaoTalk 채널 연결 테스트
# - 카카오톡에서 채널 친구 추가
# - 메시지 발송 → 응답 확인
# - /status, /help 명령 테스트
# - 음성 메시지 발송 → 텍스트 변환 확인
```

### 9.3 KakaoTalk 채널 체크리스트

- [ ] 카카오 개발자 앱 등록 및 API 키 확인
- [ ] 카카오톡 채널 생성 및 연결
- [ ] 웹훅 URL 설정 (ngrok 또는 터널 사용 가능)
- [ ] MoA 설정 파일에 API 키 입력
- [ ] Gateway 시작 후 웹훅 수신 확인
- [ ] 텍스트 메시지 송수신 테스트
- [ ] 원격 명령 (/status, /help) 테스트
- [ ] 긴 메시지 분할 전송 테스트 (1000자 초과)
- [ ] 한국어 메시지 정상 처리 확인
- [ ] 음성 메시지 수신 및 변환 테스트
- [ ] 원클릭 페어링 테스트 (새 사용자)
- [ ] 알림톡 템플릿 테스트 (비즈니스 계정)

---

## 10. 아키텍처 강점

1. **Trait 기반 확장**: 새 채널 추가 시 Channel 트레이트만 구현
2. **보안 우선 설계**: 다중 인증 레이어, 상수 시간 비교, 인증된 암호화
3. **UTF-8 안전**: 다국어 문자 처리 (한국어, 이모지 등)
4. **모듈식 음성**: 채널 로직과 분리된 음성 기능, 벤더 비종속 트랜스크립션
5. **멀티 플랫폼**: 데스크톱, 모바일, 웹, Docker, IoT 지원
6. **점진적 업데이트**: 드래프트 업데이트로 스트리밍 응답 (Telegram, Discord, Slack)

## 11. 개선 권장사항 요약

| 우선순위 | 항목 | 설명 |
|---------|------|------|
| **긴급** | wiremock 호환성 | 0.6.4로 다운그레이드 또는 Rust 업그레이드 |
| **긴급** | cargo fmt | 4개 파일 포맷 수정 필요 |
| **높음** | 토큰 TTL | Bearer 토큰 만료 정책 |
| **높음** | 웹훅 시크릿 암호화 | 설정 파일 내 시크릿 보호 강화 |
| **중간** | KakaoTalk 음성 발신 | Platform 제한 → TTS 텍스트 대안 고려 |
| **중간** | Async 뮤텍스 | 고부하 시 성능 개선 |
| **낮음** | 실시간 음성 스트리밍 | WebRTC 또는 유사 프로토콜 통합 |

---

*이 리뷰는 2026-03-03 시점의 MoA_new 코드베이스를 기반으로 작성되었습니다.*
