# SOP 구문 레퍼런스

SOP 정의는 `sops_dir` (기본값: `<workspace>/sops`) 아래의 하위 디렉터리에서 로드됩니다.

## 1. 디렉터리 레이아웃

```text
<workspace>/sops/
  deploy-prod/
    SOP.toml
    SOP.md
```

각 SOP에는 `SOP.toml`이 필수입니다. `SOP.md`는 선택 사항이지만, 파싱된 단계가 없는 실행은 검증에 실패합니다.

## 2. `SOP.toml`

```toml
[sop]
name = "deploy-prod"
description = "Deploy service to production"
version = "1.0.0"
priority = "high"              # low | normal | high | critical
execution_mode = "supervised"  # auto | supervised | step_by_step | priority_based
cooldown_secs = 300
max_concurrent = 1

[[triggers]]
type = "webhook"
path = "/sop/deploy"

[[triggers]]
type = "manual"

[[triggers]]
type = "mqtt"
topic = "ops/deploy"
condition = "$.env == \"prod\""
```

## 3. `SOP.md` 단계 형식

단계는 `## Steps` 섹션에서 파싱됩니다.

```md
## Steps

1. **Preflight** — Check service health and release window.
   - tools: http_request

2. **Deploy** — Run deployment command.
   - tools: shell
   - requires_confirmation: true
```

파서 동작:

- 번호가 매겨진 항목(`1.`, `2.`, ...)이 단계 순서를 정의합니다.
- 앞쪽 굵은 텍스트(`**Title**`)가 단계 제목이 됩니다.
- `- tools:`는 `suggested_tools`에 매핑됩니다.
- `- requires_confirmation: true`는 해당 단계에 승인을 강제합니다.

## 4. 트리거 유형

| 유형 | 필드 | 참고 |
|---|---|---|
| `manual` | 없음 | 도구 `sop_execute`로 트리거됩니다(`zeroclaw sop run` CLI 명령이 아님). |
| `webhook` | `path` | 요청 경로와 정확히 매칭 (`/sop/...` 또는 `/webhook`). |
| `mqtt` | `topic`, 선택적 `condition` | MQTT topic은 `+`와 `#` 와일드카드를 지원합니다. |
| `cron` | `expression` | 5, 6, 또는 7개 필드 지원 (5개 필드는 내부적으로 초가 앞에 추가됨). |
| `peripheral` | `board`, `signal`, 선택적 `condition` | `"{board}/{signal}"`과 매칭됩니다. |

## 5. 조건 구문

`condition`은 실패 시 안전하게 닫히도록 평가됩니다(유효하지 않은 조건/페이로드 => 매칭 없음).

- JSON 경로 비교: `$.value > 85`, `$.status == "critical"`
- 직접 숫자 비교: `> 0` (단순 페이로드에 유용)
- 연산자: `>=`, `<=`, `!=`, `>`, `<`, `==`

## 6. 검증

사용법:

```bash
zeroclaw sop validate
zeroclaw sop validate <name>
```

검증은 빈 이름/설명, 누락된 트리거, 누락된 단계, 단계 번호 매김 간격에 대해 경고합니다.
