# Nextcloud Talk 설정

이 가이드는 ZeroClaw의 네이티브 Nextcloud Talk 통합을 다룹니다.

## 1. 이 통합이 하는 일

- `POST /nextcloud-talk`을 통해 수신 Talk 봇 webhook 이벤트를 수신합니다.
- secret이 구성된 경우 webhook 서명을 검증합니다 (HMAC-SHA256).
- Nextcloud OCS API를 통해 Talk 채팅방에 봇 답변을 전송합니다.

## 2. 구성

`~/.zeroclaw/config.toml`에 다음 섹션을 추가하세요:

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"
allowed_users = ["*"]
# bot_name은 Nextcloud Talk에서 봇의 표시 이름입니다 (예: "zeroclaw").
# 봇 자신의 메시지를 무시하고 피드백 루프를 방지하는 데 사용됩니다.
# bot_name = "zeroclaw"
```

필드 참조:

- `base_url`: Nextcloud 기본 URL입니다.
- `app_token`: OCS 전송 API의 `Authorization: Bearer <token>`으로 사용되는 봇 앱 토큰입니다.
- `webhook_secret`: `X-Nextcloud-Talk-Signature` 검증을 위한 공유 secret입니다.
- `allowed_users`: 허용된 Nextcloud actor ID입니다 (`[]`는 모두 거부, `"*"`는 모두 허용).
- `bot_name`: Nextcloud Talk에서 봇의 표시 이름입니다. 설정하면 이 actor 이름의 메시지는 피드백 루프를 방지하기 위해 무시됩니다.

환경 변수 재정의:

- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET`는 설정 시 `webhook_secret`을 재정의합니다.

## 3. Gateway 엔드포인트

데몬 또는 gateway를 실행하고 webhook 엔드포인트를 노출하세요:

```bash
zeroclaw daemon
# 또는
zeroclaw gateway --host 127.0.0.1 --port 3000
```

Nextcloud Talk 봇 webhook URL을 다음으로 구성하세요:

- `https://<your-public-url>/nextcloud-talk`

## 4. 서명 검증 계약

`webhook_secret`이 구성된 경우, ZeroClaw는 다음을 검증합니다:

- 헤더 `X-Nextcloud-Talk-Random`
- 헤더 `X-Nextcloud-Talk-Signature`

검증 공식:

- `hex(hmac_sha256(secret, random + raw_request_body))`

검증에 실패하면 gateway는 `401 Unauthorized`를 반환합니다.

## 5. 메시지 라우팅 동작

- ZeroClaw는 봇에서 발생한 webhook 이벤트를 무시합니다 (`actorType = bots`).
- ZeroClaw는 비메시지/시스템 이벤트를 무시합니다.
- 답변 라우팅은 webhook 페이로드의 Talk 채팅방 토큰을 사용합니다.

## 6. 빠른 검증 체크리스트

1. 첫 번째 검증을 위해 `allowed_users = ["*"]`로 설정합니다.
2. 대상 Talk 채팅방에서 테스트 메시지를 보냅니다.
3. ZeroClaw가 같은 채팅방에서 수신하고 답변하는지 확인합니다.
4. `allowed_users`를 명시적 actor ID로 제한합니다.

## 7. 문제 해결

- `404 Nextcloud Talk not configured`: `[channels_config.nextcloud_talk]`가 누락되었습니다.
- `401 Invalid signature`: `webhook_secret`, random 헤더 또는 raw-body 서명이 불일치합니다.
- 답변 없이 webhook `200`: 이벤트가 필터링되었습니다 (봇/시스템/허용되지 않은 사용자/비메시지 페이로드).
