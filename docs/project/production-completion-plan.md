# MoA 프로덕션 완성 계획서

> 작성일: 2026-03-02
> 분석 범위: ARCHITECTURE.md 대비 전체 코드베이스 정밀 감사
> 분석 대상: 9개 핵심 항목

---

## 요약: ARCHITECTURE.md 대비 구현 상태 총괄표

| # | 항목 | ARCHITECTURE.md 요구사항 | 현재 상태 | 프로덕션 갭 |
|---|------|--------------------------|-----------|-------------|
| 1 | Tauri 임베딩 | ZeroClaw 풀 런타임이 앱에 임베딩 | ⚠️ 사이드카 방식 (별도 바이너리 실행) | 사이드카 빌드/번들 자동화 필요 |
| 2 | Gateway 동기화 릴레이 | 3-Tier 동기화 (Layer 1 TTL relay) | ⚠️ 엔드포인트 구현됨, RelayClient 미연결 | RelayClient → Gateway 와이어업 |
| 3 | 플랫폼 임베딩 | 모든 플랫폼에서 ZeroClaw 풀 런타임 | ⚠️ Android HTTP 프록시, iOS 미구현 | Android NDK 빌드, iOS 브릿지 신규 |
| 4 | SQLite + 장기기억 | SQLite + sqlite-vec + FTS5 | ✅ 완전 구현 (148+ 테스트) | sqlite-vec 미사용 (수동 벡터 → OK) |
| 5 | 동시통역 | 25언어, Gemini Live, 3-pointer 세분화 | ✅ 서버 완전 구현 (47+ 테스트) | Agent loop 미연결 (gateway-only) |
| 6 | 코딩 리뷰 파이프라인 | Claude Opus + Gemini 협업 리뷰 | ✅ 구현됨, Agent loop에서 호출 | ReviewPipeline 클래스 미사용 (인라인) |
| 7 | 채널 통신 | KakaoTalk, Telegram, Discord 등 | ✅ 33+ 채널 구현 | 통합 테스트 필요 |
| 8 | Web chat 원격 접속 | 브라우저 → MoA 실시간 채팅 | ⚠️ HTTP-only (WebSocket 미사용) | WS 스트리밍 연결 필요 |
| 9 | ARCHITECTURE.md 정합성 | 모든 설계 구현 | ⚠️ 5개 갭 존재 | 아래 상세 참조 |

---

## 항목 1: ZeroClaw Tauri 임베딩

### 현재 상태 (코드 증거)

```
clients/tauri/src-tauri/tauri.conf.json → "externalBin": ["binaries/zeroclaw"]
clients/tauri/src-tauri/lib.rs:529      → spawn_zeroclaw_gateway() 사이드카 스폰
scripts/build-tauri.sh                  → zeroclaw 바이너리 → binaries/ 복사
```

**설계 의도**: ZeroClaw는 Tauri 앱의 **사이드카 바이너리**로 번들됨. 앱 시작 시 `gateway` 명령으로 프로세스 스폰 → `http://127.0.0.1:3000` 통신. 이 방식은 ARCHITECTURE.md의 "per-device full runtime"과 일치함.

**현재 작동 여부**: ✅ 로컬 빌드 시 작동. `scripts/build-tauri.sh`가 zeroclaw 바이너리를 `binaries/zeroclaw-{target-triple}` 형식으로 복사.

### 프로덕션 미비 사항

| 항목 | 상태 | 필요 작업 |
|------|------|-----------|
| 로컬 빌드 스크립트 | ✅ 있음 | - |
| CI/CD 멀티 플랫폼 빌드 | ❌ 없음 | GitHub Actions 워크플로우 추가 |
| 코드 서명 (Windows/macOS) | ❌ 없음 | Tauri signing 설정 |
| 자동 업데이트 | ❌ 없음 | Tauri updater 플러그인 |
| 프로세스 정리 (앱 종료 시) | ⚠️ 부분 | graceful shutdown hook 추가 |
| 사이드카 실패 시 UX | ⚠️ "Starting..." 무한 | 타임아웃 + 수동 시작 다이얼로그 |

### 구현 계획

**Phase 1 — 사이드카 안정화 (1PR)**

1. `clients/tauri/src-tauri/lib.rs` 수정:
   - `spawn_zeroclaw_gateway()`에 30초 타임아웃 추가 (현재 15초)
   - 실패 시 사용자 알림 다이얼로그 (재시도/수동 시작 옵션)
   - 앱 종료 시 사이드카 프로세스 kill 로직 추가 (Tauri `on_window_event` hook)

2. `scripts/build-tauri.sh` 강화:
   - `--target` 플래그로 크로스 컴파일 지원 검증
   - Android/iOS 대상 빌드 경로 추가

**Phase 2 — CI/CD 파이프라인 (1PR)**

3. `.github/workflows/build-tauri.yml` 신규:
   - Matrix: `[windows-latest, macos-latest, ubuntu-latest]`
   - Steps: Rust 빌드 → 사이드카 복사 → Tauri 빌드 → 아티팩트 업로드
   - 코드 서명: macOS notarization, Windows authenticode (시크릿 기반)

**Phase 3 — 자동 업데이트 (1PR)**

4. Tauri updater 설정:
   - `tauri.conf.json`에 `updater` 섹션 추가
   - GitHub Releases 기반 업데이트 채널

---

## 항목 2: Gateway HTTP/WS 릴레이 엔드포인트 + Agent 동기화 초기화

### 현재 상태 (코드 증거)

**구현된 부분:**
```
src/gateway/api.rs:1032  → handle_sync_push()   ← SyncCoordinator에 위임
src/gateway/api.rs:1072  → handle_sync_pull()    ← SyncCoordinator에 위임
src/gateway/api.rs:1119  → handle_sync_status()  ← 상태 반환
src/gateway/ws.rs:36     → handle_ws_sync()      ← WebSocket 동기화
src/gateway/mod.rs:878   → 4개 라우트 등록됨
src/gateway/mod.rs:409   → SyncedMemory + SyncCoordinator 생성 (sync.enabled=true)
src/agent/loop_.rs:2445  → create_synced_memory() 호출 (sync.enabled=true)
src/sync/relay.rs:165    → RelayClient 완전 구현 (connect, store, recv)
```

**미연결 부분:**
```
RelayClient::new()  → Gateway/Agent 어디서도 호출하지 않음 (grep 결과 0건)
config.sync.relay_url → 스키마에 존재하지만 코드에서 읽히지 않음
```

### 갭 분석

현재 동기화는 **같은 프로세스 내** `SyncCoordinator`를 통해 작동함:
- Device A가 `/api/sync/push`로 delta 전송 → Device A의 coordinator가 수신
- 문제: **Device B의 Gateway는 Device A의 delta를 받을 방법이 없음**
- `RelayClient`가 이 Bridge 역할을 하도록 설계되었으나, 아직 Gateway에 와이어업되지 않음

### 구현 계획

**Phase 1 — RelayClient Wire-up (1PR, High Risk)**

파일: `src/gateway/mod.rs`

```rust
// sync.enabled && sync.relay_url이 설정된 경우:
if let Some(ref relay_url) = config.sync.relay_url {
    let relay_client = Arc::new(RelayClient::new(
        relay_url.clone(),
        coordinator.device_id().to_string(),
        config.user_id_or_default(),
    ));
    // 백그라운드에서 연결 + 수신 루프 시작
    let rc = relay_client.clone();
    let coord = coordinator.clone();
    tokio::spawn(async move {
        if let Err(e) = rc.connect().await {
            tracing::error!("Relay connection failed: {e}");
            return;
        }
        // 수신 루프: relay에서 받은 delta를 coordinator에 적용
        while let Some(entry) = rc.recv().await {
            coord.apply_remote_delta(&entry.payload).await;
        }
    });
}
```

**Phase 2 — Delta Push to Relay (1PR)**

파일: `src/memory/synced.rs` 또는 `src/sync/coordinator.rs`

- `SyncedMemory`가 delta를 기록할 때, `RelayClient.store()`도 호출하도록 연결
- 현재: delta → local journal만 기록
- 변경: delta → local journal + relay server(있으면)

**Phase 3 — 통합 테스트 (1PR)**

- 2개 SyncCoordinator + 1개 SyncRelay(인메모리)로 왕복 테스트
- Device A 메모리 변경 → Relay → Device B 수신 + 적용 검증
- 오프라인 재연결 시나리오 (Layer 2 gap detection)

---

## 항목 3: 데스크톱 + 모바일 임베딩

### 현재 상태

| 플랫폼 | 아키텍처 | 상태 |
|---------|----------|------|
| **데스크톱 (Tauri)** | 사이드카 바이너리 + HTTP 통신 | ✅ 작동 (항목 1 참조) |
| **Android** | UniFFI 브릿지 stub + HTTP 프록시 | ⚠️ HTTP 프록시로 작동하지만 NDK 네이티브 아님 |
| **iOS** | Tauri 자동생성 WKWebView (39줄) | ❌ 미구현 |

### Android 상세 분석

```
clients/android-bridge/Cargo.toml  → zeroclaw-android-bridge 크레이트
clients/android-bridge/src/lib.rs  → ZeroClawController (UniFFI 0.27)
```

**현재 동작**: Android 앱은 `ZeroClawBridge.kt`를 통해 `ZeroClawController`를 호출하지만, 실제로는 **로컬 HTTP 서버(`127.0.0.1:3000`)에 reqwest로 통신**하는 구조. zeroclaw 코어가 직접 JNI로 연결되지 않고, 별도 프로세스로 실행되어야 함.

**프로덕션 경로 (2가지 옵션)**:

| 옵션 | 설명 | 장점 | 단점 |
|------|------|------|------|
| A. 사이드카 방식 (데스크톱과 동일) | zeroclaw ARM64 바이너리 번들 → 서비스로 실행 | 코드 변경 최소 | Android NDK 빌드 필요, 바이너리 크기 |
| B. 인프로세스 임베딩 | zeroclaw를 cdylib로 빌드 → UniFFI 직접 호출 | 성능 최적, 단일 프로세스 | 대규모 리팩토링 필요 |

**권장: 옵션 A (사이드카)** — 데스크톱과 일관된 아키텍처, 최소 변경.

### iOS 상세 분석

현재 존재하는 것:
```
clients/tauri/src-tauri/gen/apple/Sources/MoAApp.swift  → WKWebView 래퍼 (39줄)
```

**프로덕션 경로**: Tauri 2 Mobile이 iOS를 공식 지원하므로, 사이드카 방식은 불가 (iOS 보안 모델). **인프로세스** 방식만 가능.

### 구현 계획

**Phase 1 — Android 사이드카 빌드 (1PR)**

1. `scripts/build-android.sh` 생성:
   ```bash
   # ARM64 + ARMv7 + x86_64 크로스 컴파일
   cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
     build --release -p zeroclaw --bin zeroclaw
   # 결과물을 Android assets 또는 jniLibs에 복사
   ```

2. `ZeroClawService.kt` 수정:
   - 앱 번들에서 zeroclaw 바이너리 추출 → 실행 권한 부여 → 포그라운드 서비스로 실행
   - 헬스체크 + 재시작 로직

3. `ZeroClawBridge.kt` 수정:
   - `initialize()` → 바이너리 추출 + 서비스 시작
   - 기존 HTTP 통신 유지 (아키텍처 변경 없음)

**Phase 2 — iOS 인프로세스 브릿지 (2PR)**

1. `clients/ios-bridge/` 크레이트 생성:
   ```toml
   [lib]
   crate-type = ["staticlib"]
   ```
   - zeroclaw 코어의 주요 함수를 C FFI로 노출
   - `start_gateway()`, `send_message()`, `get_status()`, `stop()`

2. Swift 바인딩:
   - Bridging Header로 C FFI 호출
   - SwiftUI 래퍼 (기존 Tauri WebView와 병행)

3. Xcode 프로젝트 설정:
   - `cargo-lipo`로 Universal binary (arm64 + arm64-sim)
   - `build.rs`에서 Swift Package 자동 생성

**Phase 3 — 모바일 통합 테스트 (1PR)**

- Android: APK 빌드 → 에뮬레이터 실행 → 채팅 메시지 왕복 검증
- iOS: 시뮬레이터 빌드 → 기본 기능 검증

---

## 항목 4: SQLite + 장기기억

### 현재 상태: ✅ 프로덕션 수준 완성

```
src/memory/sqlite.rs      → 1,800+ 라인 풀 구현
  - memories 테이블 + FTS5 가상 테이블 + embedding_cache 테이블
  - 자동 마이그레이션 (session_id 컬럼)
  - WAL 모드, 8MB mmap, 메모리 내 temp 테이블
  - cosine_similarity() 벡터 검색
  - BM25 키워드 검색
  - hybrid_merge() (vector 70% + keyword 30% 가중치)
  - recall() RAG 파이프라인 완전 작동
  - 148+ 테스트 케이스
```

### 검증 필요 사항

| 테스트 항목 | 방법 | 예상 결과 |
|------------|------|-----------|
| 테이블 자동 생성 | `cargo test sqlite` | ✅ Pass |
| 임베딩 계산 + 저장 | API 키 필요 (OpenAI/OpenRouter) | 실제 벡터 BLOB 저장 |
| FTS5 검색 | `cargo test fts` | ✅ Pass |
| RAG recall | `cargo test recall` | ✅ Pass |
| 세션 스코핑 | `cargo test session` | ✅ Pass |
| 캐시 LRU | `cargo test cache` | ✅ Pass |

### 추가 조치: 없음

SQLite 메모리 시스템은 **프로덕션 준비 완료**. `sqlite-vec` 확장은 미사용이지만, 수동 벡터 직렬화가 동일 기능을 제공하므로 문제 없음.

---

## 항목 5: 동시통역

### 현재 상태: ✅ 서버측 완전 구현

```
src/voice/pipeline.rs       → 25개 언어 코드, VoiceProviderKind, VoiceConfig
src/voice/gemini_live.rs    → Gemini 2.0 Flash Live API WebSocket 클라이언트
src/voice/simul.rs          → 3-pointer 세분화 엔진 (boundary/silence/length)
src/voice/simul_session.rs  → SimulSession (AudioForwarder + EventProcessor + TickTimer)
src/voice/events.rs         → VoiceEvent, ServerMessage 이벤트 스키마
src/gateway/ws.rs:647       → handle_ws_voice() WebSocket 엔드포인트
src/gateway/mod.rs:498      → VoiceSessionManager 초기화
```

### 작동 흐름

```
Client mic → WebSocket /ws/voice → SessionStart{sourceLang, targetLang}
                                 → AudioChunk{pcm16le base64}
Gateway   → SimulSession       → Gemini 2.5 Flash Live API (WebSocket)
                                 → SegmentationEngine (3-pointer)
                                 → CommitSrc / CommitTgt / AudioOut
                                 → WebSocket → Client speaker/subtitles
```

### 검증 필요 사항

| 테스트 항목 | 방법 | 예상 결과 |
|------------|------|-----------|
| 세분화 엔진 | `cargo test simul` | ✅ Pass (9+ 테스트) |
| 언어 코드 | `cargo test language` | ✅ Pass |
| 시스템 프롬프트 | `cargo test voice` | ✅ Pass |
| Gemini Live 연결 | API 키 필요, 실시간 WebSocket | Gemini 서버 연결 성공 |
| 엔드투엔드 | 클라이언트 WebSocket 연결 필요 | 음성 → 번역 → 음성 |

### 프로덕션 갭

| 항목 | 현재 | 필요 작업 |
|------|------|-----------|
| Agent loop 연결 | ❌ 미연결 | 음성은 gateway-only 설계로 Agent loop 연결 불필요 (정상) |
| 클라이언트 UI | ❌ 없음 | Tauri/Web에 마이크 입력 + 자막 표시 UI 추가 |
| 과금 | ❌ 미연결 | 세션별 Gemini API 비용 추적 |

### 구현 계획

**Phase 1 — 클라이언트 음성 UI (1PR)**

파일: `clients/tauri/src/components/VoicePanel.tsx` (신규)
- 마이크 접근 (Web Audio API / MediaRecorder)
- PCM16LE 변환 → Base64 → WebSocket `/ws/voice`
- 자막 표시 (CommitSrc/CommitTgt 이벤트)
- 번역 음성 재생 (AudioOut 이벤트)
- 언어 선택 드롭다운 (25개 언어)

---

## 항목 6: 코딩 리뷰 파이프라인 (Claude + Gemini 협업)

### 현재 상태: ✅ 구현됨, Agent Loop에서 호출됨

### 작동 방식 (구현된 코드 기준)

```
사용자 메시지 → Agent loop → LLM 응답 생성
                           → has_code_markers() 체크 (```, fn, def, class 등)
                           → config.coding.review_enabled == true ?
                              ↓ YES
                           → run_coding_review() (src/agent/loop_.rs:760-940)
                              ↓
                           ┌─ Gemini Review (1차)
                           │  POST generativelanguage.googleapis.com
                           │  temperature: 0.2, maxOutputTokens: 4096
                           │  → 아키텍처 준수 검토 + severity 분류
                           │  → ReviewReport (summary, verdict, findings)
                           ↓
                           ┌─ Claude Review (2차, enable_secondary_review=true)
                           │  POST api.anthropic.com/v1/messages
                           │  → Gemini 결과를 검증/보완
                           │  → 추가 findings 또는 기존 결과 확인
                           ↓
                           응답에 리뷰 피드백 첨부
                           "---\n**Code Review**\n" + gemini_feedback + claude_feedback
```

### 설정 (config.toml)

```toml
[coding]
review_enabled = false              # 기본 비활성화
gemini_api_key = "..."              # Google AI API 키
gemini_model = "gemini-2.5-flash"   # 1차 리뷰어
claude_api_key = "..."              # Anthropic API 키
claude_model = "claude-sonnet-4-6"  # 2차 리뷰어
enable_secondary_review = true      # 2차 리뷰 활성화
max_diff_chars = 120000             # 코드 길이 제한
```

### 사용자 체험 흐름

1. 사용자가 앱에서 **코딩 카테고리** 선택 (또는 코드 관련 질문)
2. LLM이 코드를 생성
3. 응답에 코드 마커가 포함되면 자동으로 리뷰 실행
4. Gemini가 아키텍처 관점에서 리뷰 → 결과 생성
5. Claude가 Gemini의 결과를 검증 + 구현 품질 추가 검토
6. 최종 응답 하단에 **Code Review** 섹션이 자동 첨부

### 프로덕션 갭

| 항목 | 현재 | 필요 작업 |
|------|------|-----------|
| ReviewPipeline 클래스 | ❌ 미사용 (인라인 로직) | 리팩토링 권장 (기능은 동일) |
| 카테고리 연동 | ❌ 전역 플래그만 | Coding 카테고리일 때만 자동 활성화 권장 |
| 비용 추적 | ❌ 미연결 | 리뷰 API 호출 비용을 billing에 기록 |

### 구현 계획

**Phase 1 — 리팩토링 (1PR, Low Risk)**

- `run_coding_review()`의 인라인 로직을 `ReviewPipeline::run()`으로 대체
- 동일 기능, 더 나은 테스트 가능성

**Phase 2 — 카테고리 연동 (1PR)**

- `TaskCategory::Coding`일 때 `review_enabled`를 자동 활성화하는 로직 추가
- 다른 카테고리에서는 리뷰 비활성화 (불필요한 API 호출 방지)

---

## 항목 7: 채널 통신 (카카오톡, 텔레그램 등)

### 현재 상태: ✅ 33+ 채널 구현됨

### 주요 채널 상태

| 채널 | 구현 완성도 | send() | listen() | health_check() | 테스트 |
|------|-----------|--------|----------|----------------|--------|
| **KakaoTalk** | ✅ 완전 | ✅ REST API | ✅ Webhook | ✅ 기본 | ✅ 있음 |
| **Telegram** | ✅ 완전 | ✅ Bot API | ✅ Long polling | ✅ | ✅ 있음 |
| **Discord** | ✅ 완전 | ✅ WebSocket | ✅ Gateway | ✅ | ✅ 있음 |
| **Slack** | ✅ 완전 | ✅ Web API | ✅ Polling | ✅ | ✅ 있음 |
| **LINE** | ✅ 완전 | ✅ | ✅ Webhook | ✅ | ✅ 있음 |
| **WhatsApp** | ✅ 완전 | ✅ | ✅ | ✅ | ✅ 있음 |

### KakaoTalk 상세 (커스텀 개발)

```
src/channels/kakao.rs  → ~550줄 풀 구현
  - KakaoAK Bearer 토큰 인증
  - Webhook 수신 (Axum HTTP 서버, 포트 8787)
  - REST API 송신
  - Alimtalk 템플릿 지원
  - Carousel + Button 리치 메시지
  - 한국어 인식 메시지 분할 (1000자 제한)
  - 원격 명령어 파싱 (/status, /memory 등)
```

### 검증 필요 사항

| 테스트 항목 | 방법 | 필요 조건 |
|------------|------|-----------|
| KakaoTalk 통합 | 카카오 채널 API 키 + 테스트 계정 | REST API 키, Admin 키 |
| Telegram 통합 | Bot Token + 테스트 채팅 | ZEROCLAW_TELEGRAM_TOKEN |
| Discord 통합 | Bot Token + 테스트 서버 | ZEROCLAW_DISCORD_TOKEN |
| Slack 통합 | App Token + 테스트 워크스페이스 | ZEROCLAW_SLACK_TOKEN |
| LINE 통합 | Channel Access Token | LINE 개발자 계정 |

### 구현 계획

**Phase 1 — 채널 통합 테스트 스위트 (1PR)**

파일: `tests/channel_integration.rs` (신규)
- 각 채널의 mock 서버를 사용한 send/listen 왕복 테스트
- KakaoTalk: mock Kakao API 서버 + webhook 시뮬레이션
- Telegram: mock Bot API 서버
- 에러 케이스: 네트워크 실패, 인증 만료, 메시지 제한 초과

---

## 항목 8: Web Chat 원격 MoA 접속

### 현재 상태

```
clients/web/src/lib/api.ts           → MoAClient (HTTP POST /webhook)
clients/web/src/components/ChatWidget.tsx → 완전한 채팅 UI
src/gateway/ws.rs                    → handle_ws_chat() WebSocket 구현됨
src/gateway/mod.rs:870               → GET /ws/chat 라우트 등록됨
```

**문제**: Web 클라이언트는 **HTTP POST `/webhook`만 사용** → 응답 스트리밍 없음 (전체 응답 대기).
Gateway에 `/ws/chat` WebSocket 엔드포인트가 완전 구현되어 있지만, 클라이언트가 이를 사용하지 않음.

### WebSocket 프로토콜 (구현됨)

```
Client → Server: {"type":"message","content":"Hello"}
Server → Client: {"type":"chunk","content":"Hi! "}        ← 스트리밍
Server → Client: {"type":"tool_call","name":"shell",...}   ← 도구 실행 알림
Server → Client: {"type":"tool_result","name":"shell",...} ← 도구 결과
Server → Client: {"type":"done","full_response":"..."}     ← 완료
```

### 구현 계획

**Phase 1 — Web 클라이언트 WebSocket 전환 (1PR)**

파일: `clients/web/src/lib/api.ts`
```typescript
// 기존: POST /webhook (응답 대기)
// 변경: WebSocket /ws/chat (스트리밍)

class MoAClient {
  private ws: WebSocket | null = null;

  async connectWebSocket(): Promise<void> {
    const wsUrl = this.serverUrl.replace('http', 'ws') + '/ws/chat';
    this.ws = new WebSocket(wsUrl + '?token=' + this.token);
    this.ws.onmessage = (event) => {
      const msg = JSON.parse(event.data);
      switch (msg.type) {
        case 'chunk': this.onChunk(msg.content); break;
        case 'done': this.onDone(msg.full_response); break;
        case 'tool_call': this.onToolCall(msg); break;
      }
    };
  }

  async chat(message: string): Promise<void> {
    this.ws?.send(JSON.stringify({ type: 'message', content: message }));
  }
}
```

파일: `clients/web/src/components/ChatWidget.tsx`
- 스트리밍 응답 표시 (타이핑 효과)
- 도구 실행 상태 표시 (shell, browser 등)
- 연결 상태 인디케이터 (WebSocket 연결/끊김)

**Phase 2 — Tauri 클라이언트 WebSocket 전환 (1PR)**

파일: `clients/tauri/src/lib/api.ts`
- 동일 WebSocket 프로토콜 적용
- Tauri의 경우 localhost이므로 latency 최소

---

## 항목 9: ARCHITECTURE.md 정합성 검증

### ARCHITECTURE.md 대비 불일치 사항 5개

| # | 불일치 | ARCHITECTURE.md | 현재 코드 | 심각도 |
|---|--------|-----------------|-----------|--------|
| A | 암호화 알고리즘 | AES-256-GCM + PBKDF2 | ChaCha20-Poly1305 + HKDF | Medium |
| B | RelayClient 연결 | Layer 1 TTL relay 작동 | RelayClient 미연결 | High |
| C | 과금 UI | 크레딧 구매 + 잔액 표시 | 백엔드만 존재 | Medium |
| D | iOS 네이티브 | ZeroClaw 풀 런타임 | WKWebView 래퍼만 | High |
| E | Web 채팅 스트리밍 | 실시간 브라우저 채팅 | HTTP-only (비스트리밍) | Medium |

### 불일치 A: 암호화 알고리즘

**ARCHITECTURE.md**: "AES-256-GCM 알고리즘, PBKDF2 키 파생"
**현재 코드**: `src/security/` — ChaCha20-Poly1305 + HKDF

**분석**: 두 알고리즘 모두 보안 수준 동일 (NIST 승인). ChaCha20-Poly1305는 모바일/ARM 환경에서 더 빠름.

**권장**: ARCHITECTURE.md를 현재 구현에 맞게 업데이트하되, PBKDF2 키 파생은 추가 구현 필요 (현재 랜덤 키 파일 사용).

### 불일치 B: RelayClient (항목 2에서 다룸)

### 불일치 C: 과금 UI

**ARCHITECTURE.md**: "크레딧 구매, 잔액 표시, 2x 마크업"
**현재**: `src/billing/` 백엔드 존재, 프론트엔드 UI 없음

**구현 계획**: Tauri 설정 페이지에 크레딧 잔액 표시 + 충전 링크 추가 (1PR)

### 불일치 D: iOS (항목 3에서 다룸)

### 불일치 E: Web 스트리밍 (항목 8에서 다룸)

---

## 전체 구현 로드맵 (우선순위별)

### 즉시 실행 (Critical Path)

| 순서 | 항목 | PR 범위 | 예상 변경 규모 | 위험도 |
|------|------|---------|---------------|--------|
| 1 | RelayClient Wire-up | `src/gateway/mod.rs`, `src/sync/` | M | High |
| 2 | Web WS 스트리밍 | `clients/web/src/lib/api.ts`, ChatWidget | M | Low |
| 3 | Tauri 사이드카 안정화 | `clients/tauri/src-tauri/lib.rs` | S | Medium |

### 단기 (1-2주)

| 순서 | 항목 | PR 범위 | 예상 변경 규모 | 위험도 |
|------|------|---------|---------------|--------|
| 4 | Android 사이드카 빌드 | `scripts/build-android.sh`, Service.kt | M | Medium |
| 5 | 음성 UI | `clients/tauri/src/components/VoicePanel.tsx` | M | Low |
| 6 | 채널 통합 테스트 | `tests/channel_integration.rs` | M | Low |

### 중기 (2-4주)

| 순서 | 항목 | PR 범위 | 예상 변경 규모 | 위험도 |
|------|------|---------|---------------|--------|
| 7 | iOS 인프로세스 브릿지 | `clients/ios-bridge/` (신규) | L | High |
| 8 | 코딩 리뷰 리팩토링 | `src/coding/pipeline.rs`, `src/agent/loop_.rs` | S | Low |
| 9 | CI/CD 멀티 플랫폼 | `.github/workflows/build-tauri.yml` | M | Medium |
| 10 | 과금 UI | `clients/tauri/src/components/` | S | Low |

### 장기 (4주+)

| 순서 | 항목 | PR 범위 | 위험도 |
|------|------|---------|--------|
| 11 | PBKDF2 키 파생 | `src/security/` | High |
| 12 | 자동 업데이트 | `tauri.conf.json` + updater 설정 | Medium |
| 13 | 모바일 통합 테스트 | 에뮬레이터 CI | Medium |

---

## 검증 매트릭스

각 항목 완료 후 실행해야 하는 검증:

```bash
# 기본 검증 (모든 PR)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# 동기화 변경 시
cargo test sync
cargo test synced

# 채널 변경 시
cargo test channel

# 코딩 리뷰 변경 시
cargo test coding
cargo test review

# 음성 변경 시
cargo test voice
cargo test simul

# Web 클라이언트 변경 시
cd clients/web && npm run build && npm run lint

# Tauri 변경 시
cd clients/tauri && npm run build
cd clients/tauri/src-tauri && cargo check
```

---

*이 계획서는 ARCHITECTURE.md, 전체 소스 코드, 33+ 채널 구현, 동기화 엔진, 음성 파이프라인, 코딩 리뷰 시스템, 모바일 브릿지를 정밀 분석한 결과입니다.*
