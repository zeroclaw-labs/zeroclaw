# Mattermost 통합 가이드

ZeroClaw는 Mattermost REST API v4를 통한 네이티브 통합을 지원합니다. 이 통합은 자체 호스팅, 비공개 또는 에어갭 환경에서 독립적인 커뮤니케이션이 필요한 경우에 적합합니다.

## 사전 요구사항

1.  **Mattermost 서버**: 실행 중인 Mattermost 인스턴스 (자체 호스팅 또는 클라우드).
2.  **봇 계정**:
    - **메인 메뉴 > Integrations > Bot Accounts**로 이동합니다.
    - **Add Bot Account**를 클릭합니다.
    - 사용자 이름을 설정합니다 (예: `zeroclaw-bot`).
    - **post:all** 및 **channel:read** 권한 (또는 적절한 범위)을 활성화합니다.
    - **Access Token**을 저장합니다.
3.  **Channel ID**:
    - 봇이 모니터링할 Mattermost 채널을 엽니다.
    - 채널 헤더를 클릭하고 **View Info**를 선택합니다.
    - **ID**를 복사합니다 (예: `7j8k9l...`).

## 구성

`config.toml`의 `[channels_config]` 섹션에 다음을 추가하세요:

```toml
[channels_config.mattermost]
url = "https://mm.your-domain.com"
bot_token = "your-bot-access-token"
channel_id = "your-channel-id"
allowed_users = ["user-id-1", "user-id-2"]
thread_replies = true
mention_only = true
```

### 구성 필드

| 필드 | 설명 |
|---|---|
| `url` | Mattermost 서버의 기본 URL입니다. |
| `bot_token` | 봇 계정의 Personal Access Token입니다. |
| `channel_id` | (선택사항) 수신할 채널의 ID입니다. `listen` 모드에 필요합니다. |
| `allowed_users` | (선택사항) 봇과 상호작용할 수 있는 Mattermost 사용자 ID 목록입니다. 모든 사용자를 허용하려면 `["*"]`를 사용하세요. |
| `thread_replies` | (선택사항) 최상위 사용자 메시지에 스레드로 답변할지 여부입니다. 기본값: `true`. 기존 스레드 답변은 항상 해당 스레드 내에 유지됩니다. |
| `mention_only` | (선택사항) `true`일 때, 봇 사용자 이름을 명시적으로 멘션한 메시지만 처리합니다 (예: `@zeroclaw-bot`). 기본값: `false`. |

## 스레드 대화

ZeroClaw는 두 가지 모드 모두에서 Mattermost 스레드를 지원합니다:
- 사용자가 기존 스레드에서 메시지를 보내면, ZeroClaw는 항상 같은 스레드 내에서 답변합니다.
- `thread_replies = true` (기본값)이면, 최상위 메시지는 해당 게시물에 스레드로 답변됩니다.
- `thread_replies = false`이면, 최상위 메시지는 채널 루트 레벨에서 답변됩니다.

## 멘션 전용 모드

`mention_only = true`일 때, ZeroClaw는 `allowed_users` 인증 이후 추가 필터를 적용합니다:

- 명시적 봇 멘션이 없는 메시지는 무시됩니다.
- `@bot_username`이 포함된 메시지는 처리됩니다.
- `@bot_username` 토큰은 모델에 콘텐츠를 전송하기 전에 제거됩니다.

이 모드는 불필요한 모델 호출을 줄이기 위해 활발한 공유 채널에서 유용합니다.

## 보안 참고사항

Mattermost 통합은 **독립적인 커뮤니케이션**을 위해 설계되었습니다. 자체 Mattermost 서버를 호스팅함으로써, 에이전트의 커뮤니케이션 기록이 완전히 자체 인프라 내에 유지되어 타사 클라우드 로깅을 방지합니다.
