# SOP 연결 및 이벤트 팬인

이 문서는 외부 이벤트가 SOP 실행을 트리거하는 방법을 설명합니다.

## 바로 가기

- [MQTT 통합](#2-mqtt-통합)
- [Webhook 통합](#3-webhook-통합)
- [Cron 통합](#4-cron-통합)
- [보안 기본값](#5-보안-기본값)
- [문제 해결](#6-문제-해결)

## 1. 개요

ZeroClaw는 통합 SOP 디스패처(`dispatch_sop_event`)를 통해 MQTT/webhook/cron/주변 장치 이벤트를 라우팅합니다.

주요 동작:

- **일관된 트리거 매칭:** 모든 이벤트 소스에 대해 하나의 매처 경로를 사용합니다.
- **실행 시작 감사:** 시작된 실행은 `SopAuditLogger`를 통해 저장됩니다.
- **헤드리스 안전:** agent 루프가 아닌 컨텍스트에서 `ExecuteStep` 작업은 자동으로 실행되지 않고 대기 중으로 로깅됩니다.

## 2. MQTT 통합

### 2.1 구성

`config.toml`에서 브로커 접근을 구성합니다:

```toml
[channels_config.mqtt]
broker_url = "mqtts://broker.example.com:8883"  # 평문은 mqtt:// 사용
client_id = "zeroclaw-agent-1"
topics = ["sensors/alert", "ops/deploy/#"]
qos = 1
username = "mqtt-user"      # 선택 사항
password = "mqtt-password"  # 선택 사항
use_tls = true              # 스킴과 일치해야 함 (mqtts:// => true)
```

### 2.2 트리거 정의

`SOP.toml`에서:

```toml
[[triggers]]
type = "mqtt"
topic = "sensors/alert"
condition = "$.severity >= 2"
```

MQTT 페이로드는 SOP 이벤트 페이로드(`event.payload`)로 전달된 후 단계 컨텍스트에 표시됩니다.

## 3. Webhook 통합

### 3.1 Endpoint

- **`POST /sop/{*rest}`**: SOP 전용 endpoint. 일치하는 SOP가 없으면 `404`를 반환합니다. LLM 폴백이 없습니다.
- **`POST /webhook`**: 채팅 endpoint. 먼저 SOP 디스패치를 시도하고, 일치하는 것이 없으면 일반 LLM 흐름으로 폴백합니다.

경로 매칭은 구성된 webhook 트리거 경로와 정확히 일치합니다.

예시:

- SOP의 트리거 경로: `path = "/sop/deploy"`
- 일치하는 요청: `POST /sop/deploy`

### 3.2 인증

페어링이 활성화된 경우(기본값), 다음을 제공하십시오:

1. `Authorization: Bearer <token>` (`POST /pair`에서 발급)
2. 선택적 두 번째 레이어: webhook 시크릿이 구성된 경우 `X-Webhook-Secret: <secret>`

### 3.3 멱등성

사용법:

`X-Idempotency-Key: <unique-key>`

기본값:

- TTL: 300초
- 중복 응답: `200 OK`에 `"status": "duplicate"`

멱등성 키는 endpoint별로 네임스페이스가 구분됩니다(`/webhook` vs `/sop/*`).

### 3.4 요청 예시

```bash
curl -X POST http://127.0.0.1:3000/sop/deploy \
  -H "Authorization: Bearer <token>" \
  -H "X-Idempotency-Key: $(uuidgen)" \
  -H "Content-Type: application/json" \
  -d '{"message":"deploy-service-a"}'
```

일반적인 응답:

```json
{
  "status": "accepted",
  "matched_sops": ["deploy-pipeline"],
  "source": "sop_webhook",
  "path": "/sop/deploy"
}
```

## 4. Cron 통합

스케줄러는 윈도우 기반 검사를 사용하여 캐시된 cron 트리거를 평가합니다.

- **윈도우 기반:** `(last_check, now]` 범위 내의 이벤트가 누락되지 않습니다.
- **표현식당 틱당 최대 한 번:** 하나의 폴링 윈도우에 여러 발화 시점이 있어도 디스패치는 한 번만 발생합니다.

트리거 예시:

```toml
[[triggers]]
type = "cron"
expression = "0 0 8 * * *"
```

Cron 표현식은 5, 6, 또는 7개 필드를 지원합니다.

## 5. 보안 기본값

| 기능 | 메커니즘 |
|---|---|
| **MQTT 전송** | TLS 전송을 위한 `mqtts://` + `use_tls = true` |
| **Webhook 인증** | 페어링 bearer 토큰 (기본적으로 필수), 선택적 공유 시크릿 헤더 |
| **속도 제한** | webhook 라우트의 클라이언트별 제한 (`webhook_rate_limit_per_minute`, 기본값 `60`) |
| **멱등성** | 헤더 기반 중복 제거 (`X-Idempotency-Key`, 기본 TTL `300s`) |
| **Cron 검증** | 유효하지 않은 cron 표현식은 파싱/캐시 빌드 중 안전하게 실패 |

## 6. 문제 해결

| 증상 | 원인 가능성 | 해결 방법 |
|---|---|---|
| **MQTT** 연결 오류 | 브로커 URL/TLS 불일치 | 스킴 + TLS 플래그 조합 확인 (`mqtt://`/`false`, `mqtts://`/`true`) |
| **Webhook** `401 Unauthorized` | bearer 누락 또는 유효하지 않은 시크릿 | 토큰 재발급(`POST /pair`) 및 구성된 경우 `X-Webhook-Secret` 확인 |
| **`/sop/*`에서 404 반환** | 트리거 경로 불일치 | `SOP.toml`에서 정확한 경로 사용 확인(예: `/sop/deploy`) |
| **SOP 시작되었지만 단계 미실행** | 활성 agent 루프 없이 헤드리스 트리거 | `ExecuteStep`을 위한 agent 루프 실행 또는 승인에서 일시 중지하도록 실행 설계 |
| **Cron 미발화** | daemon 미실행 또는 유효하지 않은 표현식 | `zeroclaw daemon` 실행; 로그에서 cron 파싱 경고 확인 |
