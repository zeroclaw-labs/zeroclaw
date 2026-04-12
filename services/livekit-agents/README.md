# moa 음성 대화 서비스 — LiveKit Agents 구현

이 폴더는 moa 앱의 음성 비서를 두 가지 모드로 구현한 레퍼런스 코드입니다.

```
moa_voice/
├─ requirements.txt         # 파이썬 의존성
├─ .env.example             # 환경변수 템플릿
├─ llm_factory.py           # 이용자 선택 LLM Factory (moa 핵심)
├─ custom_typecast_tts.py   # Typecast 커스텀 TTS 래퍼
├─ agent_pipeline.py        # STT → LLM → TTS 파이프라인 (프로덕션)
└─ agent_s2s.py             # Gemini Live S2S (개발·테스트 전용)
```

## 1. 설치

```bash
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
cp .env.example .env
# .env 를 실제 키로 채움
```

## 2. 두 가지 에이전트를 동시에 운영하는 방법

LiveKit Agents 워커는 하나의 `entrypoint` 당 한 종류 에이전트를 담당합니다. moa는 두 종류가 필요하므로 **두 개의 워커를 동시에 실행**하고, 클라이언트가 접속할 때 LiveKit room 이름이나 dispatch rule 로 라우팅합니다.

```bash
# 터미널 1 — 프로덕션 파이프라인 워커
python agent_pipeline.py start

# 터미널 2 — S2S 테스터 전용 워커 (internal-tester-* identity 만 허용)
python agent_s2s.py start
```

프로덕션에선 각각을 systemd / Docker / k8s Deployment 로 띄우세요. LiveKit Cloud 를 쓴다면 `lk agent deploy` CLI 한 줄로도 배포 가능합니다.

## 3. 클라이언트가 metadata 로 LLM 선택·API 키를 넘기는 방법

LiveKit 클라이언트 SDK 에서 참가자 토큰 생성 시 metadata 에 JSON 을 넣습니다.

### Swift (iOS)

```swift
let metadata = """
{
  "llm_provider": "anthropic",
  "api_key": "sk-ant-api03-...",
  "model": "claude-sonnet-4-6",
  "language": "ko",
  "tier": "premium"
}
"""

let token = AccessToken(apiKey: LIVEKIT_API_KEY, secret: LIVEKIT_API_SECRET)
token.identity = "user-\(userId)"
token.metadata = metadata
token.addGrant(VideoGrant(roomJoin: true, room: "moa-\(sessionId)"))
let jwt = try token.toJwt()

// Connect
try await room.connect(url: LIVEKIT_URL, token: jwt)
```

### Kotlin (Android)

```kotlin
val metadata = buildJsonObject {
    put("llm_provider", "google")     // 미지정이면 운영자 기본(Gemini 3.1 Flash) 사용
    put("api_key", userApiKey ?: "")  // 빈 문자열이면 운영자 키 fallback
    put("model", "gemini-3.1-flash")
    put("language", "ko")
    put("tier", if (user.isPremium) "premium" else "free")
}.toString()
```

### 웹 (JavaScript / TypeScript)

```ts
const metadata = JSON.stringify({
  llm_provider: "openai",
  api_key: userApiKey,         // 이용자 본인 키
  model: "gpt-4o",
  language: navigator.language.startsWith("ko") ? "ko" : "en",
  tier: userTier,
});

// 서버에서 토큰 발급 시 metadata 포함
const token = new AccessToken(LIVEKIT_API_KEY, LIVEKIT_API_SECRET, {
  identity: `user-${userId}`,
  metadata,
});
token.addGrant({ roomJoin: true, room: `moa-${sessionId}` });
const jwt = await token.toJwt();
```

서버에서 토큰을 발급할 때 metadata 가 JWT 에 박혀 참가자가 룸에 접속하는 순간 `participant.metadata` 로 워커에 전달됩니다. `agent_pipeline.py` 의 `entrypoint` 가 이를 파싱해 LLM 을 동적으로 생성합니다.

## 4. LLM 선택 매트릭스

| llm_provider | 기본 model | 운영자 fallback 환경변수 |
|---|---|---|
| `google` (기본) | `gemini-3.1-flash` | `GOOGLE_API_KEY` |
| `anthropic` | `claude-sonnet-4-6` | `ANTHROPIC_API_KEY` |
| `openai` | `gpt-4o` | `OPENAI_API_KEY` |

metadata 에 `api_key` 가 있으면 해당 키로, 없으면 운영자 환경변수 키로 호출됩니다. 운영자 키도 없으면 `llm_factory.parse_metadata` 가 `RuntimeError` 를 던져 세션 시작 전에 빠르게 실패합니다.

## 5. 모드 전환 시나리오

```python
# 장면 1 — 개발·내부 테스트 (2026.04 ~ 06)
# S2S Gemini Live 로 품질 확인. 한국어 체감 지연 측정.
python agent_s2s.py dev

# 장면 2 — 베타 오픈 (2026.07)
# Gemini Live 가 아직 preview → 파이프라인으로 출시
python agent_pipeline.py start
# 클라이언트는 mode 건드릴 필요 없음. 워커만 바꾸면 끝.

# 장면 3 — Gemini Live GA 이후 (2026.Q4 예상)
# 다국어 트래픽만 Gemini 로 라우팅하고 싶으면
# agent_pipeline.py 에 build_llm 분기를 추가해
# language != "ko" 인 세션에만 realtime 모델을 쓰도록 확장.
```

이 구조의 요점: **클라이언트 코드는 한 번도 바뀌지 않고**, 서버에서 돌아가는 워커와 그 안의 플러그인만 교체된다는 점입니다.

## 6. 크레딧 차감 연결 지점

`agent_pipeline.py` 의 `usage_collector` 블록이 메트릭을 모읍니다. LiveKit 이 제공하는 `metrics.UsageCollector` 는 세션 종료 시 STT / LLM / TTS 각 레이어의 토큰·초 단위 사용량을 합산해 주므로, 이 값을 moa DB 의 크레딧 테이블에 차감하는 코드만 추가하면 됩니다.

```python
async def log_usage():
    summary = usage_collector.get_summary()
    # summary 예시:
    # {
    #   "llm_prompt_tokens": 1234,
    #   "llm_completion_tokens": 567,
    #   "tts_characters_count": 890,
    #   "stt_audio_duration": 42.3
    # }
    await moa_db.deduct_credits(
        user_id=participant.identity,
        summary=summary,
        selection=selection,   # llm_factory.LLMSelection
    )
```

## 7. 한국어 품질 업그레이드 경로

1. **지금**: Cartesia Sonic-3 한국어 기본.
2. **Typecast 계약 후**: `.env` 에 `TYPECAST_API_KEY` 와 voice id 를 넣으면 자동으로 `tier="premium"` + `language="ko"` 세션에서 Typecast 가 사용됨. 코드 변경 불필요.
3. **Supertone 엔터프라이즈 계약 후**: `custom_typecast_tts.py` 를 참고해 `custom_supertone_tts.py` 를 하나 더 만들고 `build_tts` 분기 한 줄 추가.

## 8. 주의사항·알려진 제약

- **Gemini 3.1 Flash Live 의 LiveKit 제약** (2026-04 현재):
  - `generate_reply()`, `update_instructions()`, `update_chat_ctx()` 가 세션 중 무시됨. 초기 `instructions` 은 `RealtimeModel` 생성 시점에만 주입.
  - `send_client_content` 는 첫 턴 이후 1007 에러. 대화 도중 외부 컨텍스트 주입이 필요하면 파이프라인 모드로 이동.
- **Typecast REST API 스펙**은 벤더 업데이트에 따라 바뀔 수 있음. `custom_typecast_tts.py` 는 v2 REST 기준 골격이며 실전 통합 시 공식 문서 확인 필요.
- **Claude 4.6 의 prefill 금지**: LiveKit Anthropic 플러그인 1.2.x 는 ChatContext 의 마지막 메시지가 assistant 롤로 끝나면 400 에러를 낼 수 있음. 1.2.6 이상으로 유지 권장.
- **Deepgram Nova-3 의 한국어 실시간** 은 지원되지만 법률 고유명사는 `keywords=` 부스팅으로 올려야 함. 프로덕션 전 법률 용어 사전 100개 이상 등록 권장.
- **Cartesia voice id** 는 `f786b574-...` 같은 UUID 형식으로, Cartesia 대시보드에서 한국어 지원 보이스를 고른 뒤 `.env` 값 교체 필요.
