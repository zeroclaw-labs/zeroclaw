# ZeroClaw Config 레퍼런스 (운영자 중심)

주요 config 섹션과 기본값에 대한 핵심 레퍼런스입니다.

최종 검증일: **2026년 2월 21일**.

시작 시 config 경로 확인 순서:

1. `ZEROCLAW_WORKSPACE` 오버라이드 (설정된 경우)
2. 저장된 `~/.zeroclaw/active_workspace.toml` 마커 (존재하는 경우)
3. 기본값 `~/.zeroclaw/config.toml`

ZeroClaw는 시작 시 `INFO` 레벨로 확인된 config를 로그에 기록합니다:

- `Config loaded` (필드: `path`, `workspace`, `source`, `initialized`)

스키마 내보내기 명령:

- `zeroclaw config schema` (JSON Schema draft 2020-12를 stdout으로 출력)

## 핵심 키

| 키 | 기본값 | 참고 |
|---|---|---|
| `default_provider` | `openrouter` | provider ID 또는 별칭 |
| `default_model` | `anthropic/claude-sonnet-4-6` | 선택된 provider를 통해 라우팅되는 모델 |
| `default_temperature` | `0.7` | 모델 temperature |

## `[observability]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `backend` | `none` | 관찰성 백엔드: `none`, `noop`, `log`, `prometheus`, `otel`, `opentelemetry`, 또는 `otlp` |
| `otel_endpoint` | `http://localhost:4318` | backend가 `otel`일 때 사용하는 OTLP HTTP endpoint |
| `otel_service_name` | `zeroclaw` | OTLP 수집기로 전송되는 서비스 이름 |
| `runtime_trace_mode` | `none` | 런타임 트레이스 저장 모드: `none`, `rolling`, 또는 `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | 런타임 트레이스 JSONL 경로 (절대 경로가 아닌 경우 워크스페이스 기준 상대 경로) |
| `runtime_trace_max_entries` | `200` | `runtime_trace_mode = "rolling"`일 때 보존되는 최대 이벤트 수 |

참고:

- `backend = "otel"`은 non-Tokio 컨텍스트에서도 span과 메트릭을 안전하게 내보낼 수 있도록 블로킹 exporter 클라이언트를 사용하는 OTLP HTTP export를 사용합니다.
- 별칭 `opentelemetry`와 `otlp`는 동일한 OTel 백엔드에 매핑됩니다.
- 런타임 트레이스는 도구 호출 실패 및 잘못된 형식의 모델 도구 페이로드 디버깅을 위한 것입니다. 모델 출력 텍스트를 포함할 수 있으므로 공유 호스트에서는 기본적으로 비활성화하십시오.
- 런타임 트레이스 조회:
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --id <trace-id>`

예시:

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
```

## 환경 변수 Provider 오버라이드

provider 선택은 환경 변수로도 제어할 수 있습니다. 우선순위:

1. `ZEROCLAW_PROVIDER` (명시적 오버라이드, 비어 있지 않으면 항상 우선)
2. `PROVIDER` (레거시 폴백, config provider가 미설정이거나 여전히 `openrouter`인 경우에만 적용)
3. `config.toml`의 `default_provider`

컨테이너 사용자를 위한 운영 참고:

- `config.toml`에 `custom:https://.../v1`과 같은 명시적 커스텀 provider가 설정되어 있으면, Docker/컨테이너 환경의 기본 `PROVIDER=openrouter`가 더 이상 이를 대체하지 않습니다.
- 기본값이 아닌 구성된 provider를 런타임 환경으로 의도적으로 오버라이드하려면 `ZEROCLAW_PROVIDER`를 사용하십시오.

## `[agent]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `compact_context` | `true` | true일 때: bootstrap_max_chars=6000, rag_chunk_limit=2. 13B 이하 모델에 사용 |
| `max_tool_iterations` | `10` | CLI, gateway, channel에서 사용자 메시지당 최대 도구 호출 루프 턴 수 |
| `max_history_messages` | `50` | 세션당 보존되는 최대 대화 이력 메시지 수 |
| `parallel_tools` | `false` | 단일 반복 내 병렬 도구 실행 활성화 |
| `tool_dispatcher` | `auto` | 도구 디스패치 전략 |
| `tool_call_dedup_exempt` | `[]` | 턴 내 중복 호출 억제에서 제외되는 도구 이름 |
| `tool_filter_groups` | `[]` | 턴별 MCP 도구 스키마 필터 그룹 (아래 참조) |

참고:

- `max_tool_iterations = 0`을 설정하면 안전한 기본값 `10`으로 폴백합니다.
- channel 메시지가 이 값을 초과하면 런타임이 `Agent exceeded maximum tool iterations (<value>)`를 반환합니다.
- CLI, gateway, channel 도구 루프에서 대기 중인 호출이 승인 게이팅을 필요로 하지 않을 때, 여러 독립적인 도구 호출이 기본적으로 동시에 실행됩니다. 결과 순서는 안정적으로 유지됩니다.
- `parallel_tools`는 `Agent::turn()` API 표면에 적용됩니다. CLI, gateway 또는 channel 핸들러에서 사용하는 런타임 루프를 제어하지 않습니다.
- `tool_call_dedup_exempt`는 정확한 도구 이름의 배열을 허용합니다. 여기에 나열된 도구는 같은 턴에서 동일한 인수로 여러 번 호출될 수 있으며, 중복 검사를 우회합니다. 예: `tool_call_dedup_exempt = ["browser"]`.

### `tool_filter_groups`

각 턴에서 LLM에 전송되는 MCP 도구 스키마를 제한하여 턴당 토큰 오버헤드를 줄입니다. 내장(비 MCP) 도구는 항상 변경 없이 통과합니다.

각 항목은 다음 필드를 가진 테이블입니다:

| 필드 | 타입 | 용도 |
|---|---|---|
| `mode` | `"always"` \| `"dynamic"` | `always`: 도구가 무조건 포함됩니다. `dynamic`: 사용자 메시지에 키워드가 포함된 경우에만 도구가 포함됩니다. |
| `tools` | `[string]` | 도구 이름 패턴. 단일 `*` 와일드카드 지원(접두사/접미사/중위), 예: `"mcp_vikunja_*"`. |
| `keywords` | `[string]` | (dynamic 전용) 마지막 사용자 메시지와 대조되는 대소문자 무시 부분 문자열. |

`tool_filter_groups`가 비어 있으면 기능이 비활성화되고 모든 도구가 통과합니다(하위 호환 기본값).

예시:

```toml
[agent]
# Vikunja 작업 관리 MCP 도구는 항상 사용 가능합니다.
[[agent.tool_filter_groups]]
mode = "always"
tools = ["mcp_vikunja_*"]

# 브라우저 MCP 도구는 사용자 메시지에 브라우징 관련 언급이 있을 때만 포함됩니다.
[[agent.tool_filter_groups]]
mode = "dynamic"
tools = ["mcp_browser_*"]
keywords = ["browse", "navigate", "open url", "screenshot"]
```

## `[pacing]`

느린/로컬 LLM 워크로드(Ollama, llama.cpp, vLLM)를 위한 속도 제어입니다. 모든 키는 선택 사항이며, 없으면 기존 동작이 유지됩니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `step_timeout_secs` | _없음_ | 단계별 타임아웃: 단일 LLM 추론 턴의 최대 초. 전체 태스크 루프를 종료하지 않고 완전히 멈춘 모델을 감지합니다 |
| `loop_detection_min_elapsed_secs` | _없음_ | 루프 감지가 활성화되기 전 최소 경과 초. 이 임계값 미만으로 완료되는 태스크는 공격적인 루프 보호를 받고, 오래 실행되는 태스크는 유예 기간을 받습니다 |
| `loop_ignore_tools` | `[]` | 동일 출력 루프 감지에서 제외되는 도구 이름. `browser_screenshot`이 구조적으로 루프처럼 보이는 브라우저 워크플로에 유용합니다 |
| `message_timeout_scale_max` | `4` | 하드코딩된 타임아웃 스케일링 상한 오버라이드. channel 메시지 타임아웃 예산은 `message_timeout_secs * min(max_tool_iterations, message_timeout_scale_max)`입니다 |

참고:

- 이 설정은 로컬/느린 LLM 배포용입니다. 클라우드 provider 사용자는 일반적으로 필요하지 않습니다.
- `step_timeout_secs`는 전체 channel 메시지 타임아웃 예산과 독립적으로 작동합니다. 단계 타임아웃 중단은 전체 예산을 소비하지 않으며, 루프가 단순히 중지됩니다.
- `loop_detection_min_elapsed_secs`는 루프 감지 카운팅을 지연시키지, 태스크 자체를 지연시키지 않습니다. 짧은 태스크에 대한 루프 보호는 완전히 활성 상태로 유지됩니다(기본값).
- `loop_ignore_tools`는 나열된 도구에 대한 도구 출력 기반 루프 감지만 억제합니다. 다른 안전 기능(최대 반복 횟수, 전체 타임아웃)은 활성 상태로 유지됩니다.
- `message_timeout_scale_max`는 1 이상이어야 합니다. `max_tool_iterations`보다 높게 설정해도 추가 효과가 없습니다(공식이 `min()`을 사용함).
- 느린 로컬 Ollama 배포를 위한 구성 예시:

```toml
[pacing]
step_timeout_secs = 120
loop_detection_min_elapsed_secs = 60
loop_ignore_tools = ["browser_screenshot", "browser_navigate"]
message_timeout_scale_max = 8
```

## `[reliability]`

다중 모델 폴백 체인, API key 로테이션, 재시도 정책을 위한 복원력 구성입니다.

| 키 | 타입 | 기본값 | 용도 |
|---|---|---|---|
| `fallback_providers` | `[string]` | `[]` | 기본 provider 실패 시 순서대로 시도할 폴백 provider ID 목록 |
| `model_fallbacks` | `{string: [string]}` | `{}` | 모델별 폴백 체인 (모델 -> 대안 목록 매핑) |
| `api_keys` | `[string]` | `[]` | 속도 제한(429) 로테이션을 위한 추가 API key |
| `provider_retries` | `u32` | `2` | 다음 폴백으로 이동하기 전 provider당 재시도 횟수 |
| `provider_backoff_ms` | `u64` | `500` | 초기 지수 백오프 지연(밀리초) |
| `channel_initial_backoff_secs` | `u64` | `1` | channel/daemon 재시작 시도의 초기 백오프 |
| `channel_max_backoff_secs` | `u64` | `60` | channel/daemon 재시작 시도의 최대 백오프 |
| `scheduler_poll_secs` | `u64` | `5` | 스케줄러 폴링 주기(초) |
| `scheduler_retries` | `u32` | `3` | cron 작업 실행의 최대 재시도 횟수 |

참고:

- `fallback_providers`는 기본 provider가 실패(타임아웃, 연결 오류, 503, key 로테이션 후 속도 제한)할 때 순서대로 시도할 provider ID 목록입니다.
- 각 폴백 provider는 표준 확인 순서를 사용하여 독립적으로 자격 증명을 확인합니다: 명시적 config -> provider별 환경 변수 -> `ZEROCLAW_API_KEY` -> `API_KEY`.
- `model_fallbacks`는 특정 모델을 사용할 수 없을 때 의미적 폴백을 허용합니다. 예: `{ "claude-opus-4-20250514" = ["claude-sonnet-4-20250514"] }`.
- `api_keys`는 ZeroClaw가 `429`(속도 제한) 응답 시 순환하는 추가 API key를 제공합니다. 기본 `api_key`(전역 또는 채널별 설정)가 먼저 시도됩니다.
- `provider_retries`는 각 폴백 시도 전에 적용됩니다. `provider_retries = 2`이고 `provider_backoff_ms = 500`이면 런타임은 500ms, 1000ms 지연으로 재시도합니다.
- `channel_initial_backoff_secs`와 `channel_max_backoff_secs`는 일시적 장애 후 channel 재연결의 지수 백오프를 제어합니다.
- `scheduler_poll_secs`는 내장 스케줄러가 cron 트리거 태스크를 확인하는 빈도를 제어합니다.
- `scheduler_retries`는 실패한 예약 태스크 실행의 재시도 횟수를 제한합니다.
- 핫 리로드 지원: 이 섹션의 업데이트는 재시작 없이 다음 channel 메시지 또는 provider 요청에서 적용됩니다.

예시:

```toml
[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
api_keys = ["sk-backup-1", "sk-backup-2"]

[reliability.model_fallbacks]
"claude-opus-4-20250514" = ["claude-sonnet-4-20250514"]
"gpt-4o" = ["gpt-4-turbo", "gpt-3.5-turbo"]

provider_retries = 3
provider_backoff_ms = 1000
channel_initial_backoff_secs = 2
channel_max_backoff_secs = 120
scheduler_poll_secs = 10
scheduler_retries = 5
```

폴백 트리거:

- **타임아웃**: provider 타임아웃 윈도우 내 응답 없음.
- **연결 오류**: 네트워크/DNS 장애.
- **서비스 불가(503)**: provider 일시적 장애.
- **속도 제한(429)**: 먼저 동일 provider/모델에서 `api_keys`를 순환한 후, 다음 provider로 폴백.
- **모델 미발견**: 해당 모델에 `model_fallbacks`가 구성되어 있으면 순서대로 대안을 시도.

폴백이 트리거되지 **않는** 경우:

- **클라이언트 오류(400)**: 잘못된 요청. 재시도해도 도움이 되지 않습니다.
- **유효하지 않은 자격 증명(401/403)**: 영구적 인증 실패.
- **모델 출력 오류**: provider가 응답했지만 모델이 응답에서 오류를 반환한 경우.

자세한 구성 지침은 [다중 모델 설정 및 폴백 체인](/docs/getting-started/multi-model-setup.md)을 참조하십시오.

## `[security.otp]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 민감한 작업/도메인에 대한 OTP 게이팅 활성화 |
| `method` | `totp` | OTP 방식 (`totp`, `pairing`, `cli-prompt`) |
| `token_ttl_secs` | `30` | TOTP 시간 단계 윈도우(초) |
| `cache_valid_secs` | `300` | 최근 검증된 OTP 코드의 캐시 윈도우 |
| `gated_actions` | `["shell","file_write","browser_open","browser","memory_forget"]` | OTP로 보호되는 도구 작업 |
| `gated_domains` | `[]` | OTP가 필요한 명시적 도메인 패턴 (`*.example.com`, `login.example.com`) |
| `gated_domain_categories` | `[]` | 도메인 프리셋 카테고리 (`banking`, `medical`, `government`, `identity_providers`) |

참고:

- 도메인 패턴은 와일드카드 `*`를 지원합니다.
- 카테고리 프리셋은 검증 시 선별된 도메인 세트로 확장됩니다.
- 유효하지 않은 도메인 glob 또는 알 수 없는 카테고리는 시작 시 즉시 실패합니다.
- `enabled = true`이고 OTP 시크릿이 없으면, ZeroClaw가 하나를 생성하고 등록 URI를 한 번 출력합니다.

예시:

```toml
[security.otp]
enabled = true
method = "totp"
token_ttl_secs = 30
cache_valid_secs = 300
gated_actions = ["shell", "browser_open"]
gated_domains = ["*.chase.com", "accounts.google.com"]
gated_domain_categories = ["banking"]
```

## `[security.estop]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 비상 정지 상태 머신 및 CLI 활성화 |
| `state_file` | `~/.zeroclaw/estop-state.json` | 영구 estop 상태 경로 |
| `require_otp_to_resume` | `true` | 재개 작업 전 OTP 검증 필요 |

참고:

- estop 상태는 원자적으로 저장되고 시작 시 다시 로드됩니다.
- 손상되거나 읽을 수 없는 estop 상태는 안전하게 `kill_all`로 폴백합니다.
- CLI 명령 `zeroclaw estop`으로 활성화하고 `zeroclaw estop resume`으로 레벨을 해제합니다.

## `[agents.<name>]`

위임 하위 agent 구성입니다. `[agents]` 아래의 각 키는 기본 agent가 위임할 수 있는 명명된 하위 agent를 정의합니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `provider` | _필수_ | provider 이름 (예: `"ollama"`, `"openrouter"`, `"anthropic"`) |
| `model` | _필수_ | 하위 agent의 모델 이름 |
| `system_prompt` | 미설정 | 하위 agent의 선택적 시스템 프롬프트 오버라이드 |
| `api_key` | 미설정 | 선택적 API key 오버라이드 (`secrets.encrypt = true`일 때 암호화되어 저장) |
| `temperature` | 미설정 | 하위 agent의 temperature 오버라이드 |
| `max_depth` | `3` | 중첩 위임의 최대 재귀 깊이 |
| `agentic` | `false` | 하위 agent의 다중 턴 도구 호출 루프 모드 활성화 |
| `allowed_tools` | `[]` | agentic 모드의 도구 allowlist |
| `max_iterations` | `10` | agentic 모드의 최대 도구 호출 반복 횟수 |
| `timeout_secs` | `120` | 비 agentic provider 호출의 타임아웃(초, 1-3600) |
| `agentic_timeout_secs` | `300` | agentic 하위 agent 루프의 타임아웃(초, 1-3600) |
| `skills_directory` | 미설정 | 범위 지정 skill 로딩을 위한 선택적 skill 디렉터리 경로 (워크스페이스 기준 상대 경로) |

참고:

- `agentic = false`는 기존의 단일 프롬프트->응답 위임 동작을 유지합니다.
- `agentic = true`는 `allowed_tools`에 최소 하나 이상의 일치하는 항목이 필요합니다.
- `delegate` 도구는 재진입 위임 루프를 방지하기 위해 하위 agent allowlist에서 제외됩니다.
- 하위 agent는 다음을 포함하는 강화된 시스템 프롬프트를 받습니다: 도구 섹션(허용된 도구 및 매개변수), skill 섹션(범위 지정 또는 기본 디렉터리), 워크스페이스 경로, 현재 날짜/시간, 안전 제약 조건, `shell`이 유효 도구 목록에 있을 때 셸 정책.
- `skills_directory`가 미설정이거나 비어 있으면, 하위 agent는 기본 워크스페이스 `skills/` 디렉터리에서 skill을 로드합니다. 설정된 경우 해당 디렉터리(워크스페이스 루트 기준 상대 경로)에서만 skill을 로드하여 agent별 범위 지정 skill 세트를 사용할 수 있습니다.

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2
agentic = true
allowed_tools = ["web_search", "http_request", "file_read"]
max_iterations = 8
agentic_timeout_secs = 600

[agents.coder]
provider = "ollama"
model = "qwen2.5-coder:32b"
temperature = 0.2
timeout_secs = 60

[agents.code_reviewer]
provider = "anthropic"
model = "claude-opus-4-5"
system_prompt = "You are an expert code reviewer focused on security and performance."
agentic = true
allowed_tools = ["file_read", "shell"]
skills_directory = "skills/code-review"
```

## `[runtime]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `reasoning_enabled` | 미설정 (`None`) | 명시적 제어를 지원하는 provider에 대한 전역 추론/사고 오버라이드 |

참고:

- `reasoning_enabled = false`는 지원되는 provider(현재 `ollama`, 요청 필드 `think: false` 사용)에 대해 provider 측 추론을 명시적으로 비활성화합니다.
- `reasoning_enabled = true`는 지원되는 provider에 대해 추론을 명시적으로 요청합니다(`ollama`에서 `think: true`).
- 미설정 시 provider 기본값을 유지합니다.

## `[skills]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `open_skills_enabled` | `false` | 커뮤니티 `open-skills` 저장소의 옵트인 로딩/동기화 |
| `open_skills_dir` | 미설정 | `open-skills`의 선택적 로컬 경로 (활성화 시 기본값: `$HOME/open-skills`) |
| `prompt_injection_mode` | `full` | Skill 프롬프트 상세 수준: `full` (인라인 지시사항/도구) 또는 `compact` (이름/설명/위치만) |

참고:

- 보안 우선 기본값: `open_skills_enabled = true`가 아닌 한 ZeroClaw는 `open-skills`를 클론하거나 동기화하지 **않습니다**.
- 환경 변수 오버라이드:
  - `ZEROCLAW_OPEN_SKILLS_ENABLED`는 `1/0`, `true/false`, `yes/no`, `on/off`를 허용합니다.
  - `ZEROCLAW_OPEN_SKILLS_DIR`는 비어 있지 않을 때 저장소 경로를 오버라이드합니다.
  - `ZEROCLAW_SKILLS_PROMPT_MODE`는 `full` 또는 `compact`를 허용합니다.
- 활성화 플래그 우선순위: `ZEROCLAW_OPEN_SKILLS_ENABLED` -> `config.toml`의 `skills.open_skills_enabled` -> 기본값 `false`.
- `prompt_injection_mode = "compact"`는 시작 프롬프트 크기를 줄이면서 skill 파일을 요청 시 사용할 수 있도록 하기 위해 저컨텍스트 로컬 모델에 권장됩니다.
- Skill 로딩과 `zeroclaw skills install` 모두 정적 보안 감사를 적용합니다. 심볼릭 링크, 스크립트 유형 파일, 고위험 셸 페이로드 스니펫, 안전하지 않은 마크다운 링크 탐색을 포함하는 skill은 거부됩니다.

## `[composio]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | Composio 관리형 OAuth 도구 활성화 |
| `api_key` | 미설정 | `composio` 도구에서 사용하는 Composio API key |
| `entity_id` | `default` | connect/execute 호출에 전송되는 기본 `user_id` |

참고:

- 하위 호환성: 레거시 `enable = true`는 `enabled = true`의 별칭으로 허용됩니다.
- `enabled = false`이거나 `api_key`가 없으면 `composio` 도구가 등록되지 않습니다.
- ZeroClaw는 `toolkit_versions=latest`로 Composio v3 도구를 요청하고 `version="latest"`로 도구를 실행하여 오래된 기본 도구 리비전을 방지합니다.
- 일반적인 흐름: `connect` 호출, 브라우저 OAuth 완료, 그런 다음 원하는 도구 작업에 대해 `execute` 실행.
- Composio가 누락된 연결 계정 참조 오류를 반환하면, `list_accounts`(선택적으로 `app` 포함)를 호출하고 반환된 `connected_account_id`를 `execute`에 전달하십시오.

## `[cost]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 비용 추적 활성화 |
| `daily_limit_usd` | `10.00` | 일일 지출 한도(USD) |
| `monthly_limit_usd` | `100.00` | 월간 지출 한도(USD) |
| `warn_at_percent` | `80` | 지출이 한도의 이 비율에 도달하면 경고 |
| `allow_override` | `false` | `--override` 플래그로 예산 초과 요청 허용 |

참고:

- `enabled = true`이면 런타임이 요청당 비용 추정치를 추적하고 일일/월간 한도를 적용합니다.
- `warn_at_percent` 임계값에서 경고가 발생하지만 요청은 계속됩니다.
- 한도에 도달하면 `allow_override = true`이고 `--override` 플래그가 전달되지 않는 한 요청이 거부됩니다.

## `[identity]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `format` | `openclaw` | ID 형식: `"openclaw"` (기본값) 또는 `"aieos"` |
| `aieos_path` | 미설정 | AIEOS JSON 파일 경로 (워크스페이스 기준 상대 경로) |
| `aieos_inline` | 미설정 | 인라인 AIEOS JSON (파일 경로 대안) |

참고:

- AIEOS / OpenClaw ID 문서를 로드하려면 `format = "aieos"`와 함께 `aieos_path` 또는 `aieos_inline`을 사용하십시오.
- `aieos_path`와 `aieos_inline` 중 하나만 설정해야 합니다. `aieos_path`가 우선합니다.

## `[multimodal]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `max_images` | `4` | 요청당 허용되는 최대 이미지 마커 수 |
| `max_image_size_mb` | `5` | base64 인코딩 전 이미지당 크기 제한 |
| `allow_remote_fetch` | `false` | 마커에서 `http(s)` 이미지 URL 가져오기 허용 |

참고:

- 런타임은 사용자 메시지에서 ``[IMAGE:<source>]`` 구문의 이미지 마커를 허용합니다.
- 지원되는 소스:
  - 로컬 파일 경로 (예: ``[IMAGE:/tmp/screenshot.png]``)
- Data URI (예: ``[IMAGE:data:image/png;base64,...]``)
- 원격 URL (`allow_remote_fetch = true`인 경우에만)
- 허용되는 MIME 타입: `image/png`, `image/jpeg`, `image/webp`, `image/gif`, `image/bmp`.
- 활성 provider가 비전을 지원하지 않으면, 이미지를 무시하는 대신 구조화된 기능 오류(`capability=vision`)와 함께 요청이 실패합니다.

## `[browser]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | `browser_open` 도구 활성화 (스크래핑 없이 시스템 브라우저에서 URL 열기) |
| `allowed_domains` | `[]` | `browser_open`에 허용되는 도메인 (정확/하위 도메인 매칭, 또는 모든 공용 도메인에 `"*"`) |
| `session_name` | 미설정 | 브라우저 세션 이름 (agent-browser 자동화용) |
| `backend` | `agent_browser` | 브라우저 자동화 백엔드: `"agent_browser"`, `"rust_native"`, `"computer_use"`, 또는 `"auto"` |
| `native_headless` | `true` | rust-native 백엔드의 헤드리스 모드 |
| `native_webdriver_url` | `http://127.0.0.1:9515` | rust-native 백엔드의 WebDriver endpoint URL |
| `native_chrome_path` | 미설정 | rust-native 백엔드의 선택적 Chrome/Chromium 실행 파일 경로 |

### `[browser.computer_use]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `endpoint` | `http://127.0.0.1:8787/v1/actions` | computer-use 작업(OS 수준 마우스/키보드/스크린샷)을 위한 사이드카 endpoint |
| `api_key` | 미설정 | computer-use 사이드카용 선택적 bearer 토큰 (암호화되어 저장) |
| `timeout_ms` | `15000` | 작업당 요청 타임아웃(밀리초) |
| `allow_remote_endpoint` | `false` | computer-use 사이드카에 원격/공용 endpoint 허용 |
| `window_allowlist` | `[]` | 사이드카 정책에 전달되는 선택적 창 제목/프로세스 allowlist |
| `max_coordinate_x` | 미설정 | 좌표 기반 작업의 선택적 X축 경계 |
| `max_coordinate_y` | 미설정 | 좌표 기반 작업의 선택적 Y축 경계 |

참고:

- `backend = "computer_use"`일 때, agent는 브라우저 작업을 `computer_use.endpoint`의 사이드카에 위임합니다.
- `allow_remote_endpoint = false`(기본값)는 우발적 공용 노출을 방지하기 위해 루프백이 아닌 endpoint를 거부합니다.
- `window_allowlist`를 사용하여 사이드카가 상호 작용할 수 있는 OS 창을 제한하십시오.

## `[http_request]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | API 상호 작용을 위한 `http_request` 도구 활성화 |
| `allowed_domains` | `[]` | HTTP 요청에 허용되는 도메인 (정확/하위 도메인 매칭, 또는 모든 공용 도메인에 `"*"`) |
| `max_response_size` | `1000000` | 최대 응답 크기(바이트, 기본: 1 MB) |
| `timeout_secs` | `30` | 요청 타임아웃(초) |

참고:

- 기본 거부: `allowed_domains`가 비어 있으면 모든 HTTP 요청이 거부됩니다.
- 정확한 도메인 또는 하위 도메인 매칭을 사용하십시오(예: `"api.example.com"`, `"example.com"`). 또는 모든 공용 도메인을 허용하려면 `"*"`를 사용하십시오.
- `"*"`가 구성되어 있어도 로컬/프라이빗 대상은 여전히 차단됩니다.

## `[google_workspace]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | `google_workspace` 도구 활성화 |
| `credentials_path` | 미설정 | Google 서비스 계정 또는 OAuth 자격 증명 JSON 경로 |
| `default_account` | 미설정 | `gws`에 `--account`로 전달되는 기본 Google 계정 |
| `allowed_services` | (내장 목록) | agent가 접근할 수 있는 서비스: `drive`, `gmail`, `calendar`, `sheets`, `docs`, `slides`, `tasks`, `people`, `chat`, `classroom`, `forms`, `keep`, `meet`, `events` |
| `rate_limit_per_minute` | `60` | 분당 최대 `gws` 호출 수 |
| `timeout_secs` | `30` | 호출당 실행 타임아웃(초) |
| `audit_log` | `false` | 모든 `gws` 호출에 대해 `INFO` 로그 라인 출력 |

### `[[google_workspace.allowed_operations]]`

이 배열이 비어 있지 않으면 정확히 일치하는 항목만 통과합니다. 항목은 `service`, `resource`, `sub_resource`, `method`가 모두 일치할 때 호출과 매칭됩니다. 배열이 비어 있으면(기본값) `allowed_services` 내의 모든 조합이 사용 가능합니다.

| 키 | 필수 | 용도 |
|---|---|---|
| `service` | 예 | 서비스 식별자 (`allowed_services`의 항목과 일치해야 함) |
| `resource` | 예 | 최상위 리소스 이름 (Gmail의 경우 `users`, Drive의 경우 `files`, Calendar의 경우 `events`) |
| `sub_resource` | 아니오 | 4세그먼트 gws 명령의 하위 리소스. Gmail 작업은 `gws gmail users <sub_resource> <method>`를 사용하므로, Gmail 항목은 런타임에 매칭하려면 `sub_resource`가 필요합니다. Drive, Calendar 및 대부분의 다른 서비스는 3세그먼트 명령을 사용하며 생략합니다. |
| `methods` | 예 | 해당 resource/sub_resource에서 허용되는 하나 이상의 메서드 이름 |

Gmail은 모든 작업에 `gws gmail users <sub_resource> <method>`를 사용합니다. `sub_resource` 없는 Gmail 항목은 런타임에 매칭되지 않습니다. Drive와 Calendar는 3세그먼트 명령을 사용하며 `sub_resource`를 생략합니다.

```toml
[google_workspace]
enabled = true
default_account = "owner@company.com"
allowed_services = ["gmail"]
audit_log = true

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "messages"
methods = ["list", "get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "drafts"
methods = ["list", "get", "create", "update"]
```

참고:

- `gws`가 설치되고 인증되어 있어야 합니다(`gws auth login`). 설치: `npm install -g @googleworkspace/cli`.
- `credentials_path`는 각 호출 전에 `GOOGLE_APPLICATION_CREDENTIALS`를 설정합니다.
- `allowed_services`는 생략하거나 비어 있으면 내장 목록을 기본값으로 사용합니다.
- 검증은 중복 `(service, resource)` 쌍과 단일 항목 내 중복 메서드를 거부합니다.
- 전체 정책 모델 및 검증된 워크플로 예시는 `docs/superpowers/specs/2026-03-19-google-workspace-operation-allowlist.md`를 참조하십시오.

## `[gateway]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `host` | `127.0.0.1` | 바인드 주소 |
| `port` | `42617` | gateway 수신 포트 |
| `require_pairing` | `true` | bearer 인증 전 페어링 필요 |
| `allow_public_bind` | `false` | 우발적 공용 노출 차단 |
| `path_prefix` | _(없음)_ | 리버스 프록시 배포를 위한 URL 경로 접두사 (예: `"/zeroclaw"`) |

ZeroClaw를 하위 경로에 매핑하는 리버스 프록시 뒤에 배포할 때, `path_prefix`를 해당 하위 경로로 설정하십시오(예: `"/zeroclaw"`). 모든 gateway 라우트가 이 접두사 아래에서 제공됩니다. 값은 `/`로 시작해야 하며 `/`로 끝나서는 안 됩니다.

## `[autonomy]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `level` | `supervised` | `read_only`, `supervised`, 또는 `full` |
| `workspace_only` | `true` | 명시적으로 비활성화하지 않는 한 절대 경로 입력을 거부 |
| `allowed_commands` | _셸 실행에 필수_ | 허용된 실행 파일 이름, 명시적 실행 파일 경로, 또는 `"*"` |
| `forbidden_paths` | 내장 보호 목록 | 명시적 경로 거부 목록 (기본적으로 시스템 경로 + 민감한 dotdir) |
| `allowed_roots` | `[]` | 정규화 후 워크스페이스 외부에서 허용되는 추가 루트 |
| `max_actions_per_hour` | `20` | 정책당 작업 예산 |
| `max_cost_per_day_cents` | `500` | 정책당 지출 가드레일 |
| `require_approval_for_medium_risk` | `true` | 중간 위험 명령에 대한 승인 게이트 |
| `block_high_risk_commands` | `true` | 고위험 명령에 대한 하드 블록 |
| `auto_approve` | `[]` | 항상 자동 승인되는 도구 작업 |
| `always_ask` | `[]` | 항상 승인이 필요한 도구 작업 |

참고:

- `level = "full"`은 셸 실행에 대한 중간 위험 승인 게이팅을 건너뛰지만, 구성된 가드레일은 계속 적용합니다.
- 워크스페이스 외부 접근은 `workspace_only = false`여도 `allowed_roots`가 필요합니다.
- `allowed_roots`는 절대 경로, `~/...`, 워크스페이스 기준 상대 경로를 지원합니다.
- `allowed_commands` 항목은 명령 이름(예: `"git"`), 명시적 실행 파일 경로(예: `"/usr/bin/antigravity"`), 또는 모든 명령 이름/경로를 허용하는 `"*"`(위험 게이트는 여전히 적용)일 수 있습니다.
- 셸 구분자/연산자 파싱은 따옴표를 인식합니다. 따옴표 안의 `;`와 같은 문자는 명령 구분자가 아닌 리터럴로 처리됩니다.
- 따옴표 없는 셸 체이닝/연산자는 여전히 정책 검사에 의해 적용됩니다(`;`, `|`, `&&`, `||`, 백그라운드 체이닝, 리디렉션).

```toml
[autonomy]
workspace_only = false
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
allowed_roots = ["~/Desktop/projects", "/opt/shared-repo"]
```

## `[memory]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `backend` | `sqlite` | `sqlite`, `lucid`, `markdown`, `none` |
| `auto_save` | `true` | 사용자가 명시한 입력만 저장 (어시스턴트 출력은 제외) |
| `embedding_provider` | `none` | `none`, `openai`, 또는 커스텀 endpoint |
| `embedding_model` | `text-embedding-3-small` | 임베딩 모델 ID, 또는 `hint:<name>` 라우트 |
| `embedding_dimensions` | `1536` | 선택된 임베딩 모델의 예상 벡터 크기 |
| `vector_weight` | `0.7` | 하이브리드 랭킹 벡터 가중치 |
| `keyword_weight` | `0.3` | 하이브리드 랭킹 키워드 가중치 |

참고:

- 메모리 컨텍스트 주입은 레거시 `assistant_resp*` 자동 저장 키를 무시하여 이전 모델 작성 요약이 사실로 취급되는 것을 방지합니다.

## `[[model_routes]]` 및 `[[embedding_routes]]`

모델 ID가 변경되어도 통합이 안정적인 이름을 유지할 수 있도록 라우트 힌트를 사용합니다.

### `[[model_routes]]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `hint` | _필수_ | 태스크 힌트 이름 (예: `"reasoning"`, `"fast"`, `"code"`, `"summarize"`) |
| `provider` | _필수_ | 라우팅할 provider (알려진 provider 이름과 일치해야 함) |
| `model` | _필수_ | 해당 provider에서 사용할 모델 |
| `api_key` | 미설정 | 이 라우트 provider의 선택적 API key 오버라이드 |

### `[[embedding_routes]]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `hint` | _필수_ | 라우트 힌트 이름 (예: `"semantic"`, `"archive"`, `"faq"`) |
| `provider` | _필수_ | 임베딩 provider (`"none"`, `"openai"`, 또는 `"custom:<url>"`) |
| `model` | _필수_ | 해당 provider에서 사용할 임베딩 모델 |
| `dimensions` | 미설정 | 이 라우트의 선택적 임베딩 차원 오버라이드 |
| `api_key` | 미설정 | 이 라우트 provider의 선택적 API key 오버라이드 |

```toml
[memory]
embedding_model = "hint:semantic"

[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "provider/model-id"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

업그레이드 전략:

1. 힌트를 안정적으로 유지합니다(`hint:reasoning`, `hint:semantic`).
2. 라우트 항목에서 `model = "...new-version..."`만 업데이트합니다.
3. 재시작/롤아웃 전에 `zeroclaw doctor`로 검증합니다.

자연어 config 경로:

- 일반 agent 채팅 중 어시스턴트에게 평문으로 라우트 재구성을 요청할 수 있습니다.
- 런타임은 수동 TOML 편집 없이 `model_routing_config` 도구를 통해 이러한 업데이트를 저장할 수 있습니다(기본값, 시나리오, 위임 하위 agent).

요청 예시:

- `Set conversation to provider kimi, model moonshot-v1-8k.`
- `Set coding to provider openai, model gpt-5.3-codex, and auto-route when message contains code blocks.`
- `Create a coder sub-agent using openai/gpt-5.3-codex with tools file_read,file_write,shell.`

## `[query_classification]`

자동 모델 힌트 라우팅 -- 콘텐츠 패턴을 기반으로 사용자 메시지를 `[[model_routes]]` 힌트에 매핑합니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 자동 쿼리 분류 활성화 |
| `rules` | `[]` | 분류 규칙 (우선순위 순으로 평가) |

`rules`의 각 규칙:

| 키 | 기본값 | 용도 |
|---|---|---|
| `hint` | _필수_ | `[[model_routes]]` 힌트 값과 일치해야 함 |
| `keywords` | `[]` | 대소문자 무시 부분 문자열 매칭 |
| `patterns` | `[]` | 대소문자 구분 리터럴 매칭 (코드 펜스, `"fn "` 같은 키워드용) |
| `min_length` | 미설정 | 메시지 길이가 N자 이상인 경우에만 매칭 |
| `max_length` | 미설정 | 메시지 길이가 N자 이하인 경우에만 매칭 |
| `priority` | `0` | 높은 우선순위 규칙이 먼저 검사됨 |

```toml
[query_classification]
enabled = true

[[query_classification.rules]]
hint = "reasoning"
keywords = ["explain", "analyze", "why"]
min_length = 200
priority = 10

[[query_classification.rules]]
hint = "fast"
keywords = ["hi", "hello", "thanks"]
max_length = 50
priority = 5
```

## `[channels_config]`

최상위 channel 옵션은 `channels_config` 아래에 구성됩니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `message_timeout_secs` | `300` | channel 메시지 처리의 기본 타임아웃(초). 런타임이 도구 루프 깊이에 따라 스케일링합니다(최대 4배, `[pacing].message_timeout_scale_max`로 오버라이드 가능) |

예시:

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.linq]`
- `[channels_config.nextcloud_talk]`
- `[channels_config.email]`
- `[channels_config.nostr]`

참고:

- 기본 `300s`는 클라우드 API보다 느린 온디바이스 LLM(Ollama)에 최적화되어 있습니다.
- 런타임 타임아웃 예산은 `message_timeout_secs * scale`이며, `scale = min(max_tool_iterations, cap)`이고 최소값은 `1`입니다. 기본 상한은 `4`이며 `[pacing].message_timeout_scale_max`로 오버라이드할 수 있습니다.
- 이 스케일링은 첫 번째 LLM 턴이 느리거나 재시도되더라도 이후 도구 루프 턴이 완료될 수 있도록 잘못된 타임아웃을 방지합니다.
- 클라우드 API(OpenAI, Anthropic 등)를 사용하는 경우 `60` 이하로 줄일 수 있습니다.
- `30` 미만의 값은 즉각적인 타임아웃 반복을 방지하기 위해 `30`으로 클램핑됩니다.
- 타임아웃이 발생하면 사용자에게 다음 메시지가 표시됩니다: `⚠️ Request timed out while waiting for the model. Please try again.`
- Telegram 전용 중단 동작은 `channels_config.telegram.interrupt_on_new_message`(기본 `false`)로 제어됩니다.
  활성화 시, 같은 채팅에서 같은 발신자의 최신 메시지가 진행 중인 요청을 취소하고 중단된 사용자 컨텍스트를 보존합니다.
- `zeroclaw channel start` 실행 중 `default_provider`, `default_model`, `default_temperature`, `api_key`, `api_url`, `reliability.*` 업데이트는 다음 인바운드 메시지에서 `config.toml`로부터 핫 적용됩니다.

### `[channels_config.nostr]`

| 키 | 기본값 | 용도 |
|---|---|---|
| `private_key` | _필수_ | Nostr 비공개 키 (hex 또는 `nsec1…` bech32). `secrets.encrypt = true`일 때 저장 시 암호화 |
| `relays` | 참고 참조 | relay WebSocket URL 목록. 기본값: `relay.damus.io`, `nos.lol`, `relay.primal.net`, `relay.snort.social` |
| `allowed_pubkeys` | `[]` (모두 거부) | 발신자 allowlist (hex 또는 `npub1…`). `"*"`로 모든 발신자 허용 |

참고:

- NIP-04 (레거시 암호화 DM)와 NIP-17 (선물 포장 비공개 메시지)를 모두 지원합니다. 응답은 발신자의 프로토콜을 자동으로 따릅니다.
- `private_key`는 고가치 시크릿입니다. 프로덕션에서는 `secrets.encrypt = true`(기본값)를 유지하십시오.

자세한 channel 매트릭스와 allowlist 동작은 [channels-reference.md](channels-reference.md)를 참조하십시오.

### `[channels_config.whatsapp]`

WhatsApp은 하나의 config 테이블 아래에 두 가지 백엔드를 지원합니다.

Cloud API 모드 (Meta webhook):

| 키 | 필수 | 용도 |
|---|---|---|
| `access_token` | 예 | Meta Cloud API bearer 토큰 |
| `phone_number_id` | 예 | Meta 전화번호 ID |
| `verify_token` | 예 | webhook 검증 토큰 |
| `app_secret` | 선택 | webhook 서명 검증 활성화 (`X-Hub-Signature-256`) |
| `allowed_numbers` | 권장 | 허용된 인바운드 번호 (`[]` = 모두 거부, `"*"` = 모두 허용) |

WhatsApp Web 모드 (네이티브 클라이언트):

| 키 | 필수 | 용도 |
|---|---|---|
| `session_path` | 예 | 영구 SQLite 세션 경로 |
| `pair_phone` | 선택 | 페어 코드 플로우 전화번호 (숫자만) |
| `pair_code` | 선택 | 커스텀 페어 코드 (미설정 시 자동 생성) |
| `allowed_numbers` | 권장 | 허용된 인바운드 번호 (`[]` = 모두 거부, `"*"` = 모두 허용) |
| `mention_only` | 선택 | `true`일 때, 봇이 @멘션된 그룹 메시지만 응답 (DM은 항상 처리) |

참고:

- WhatsApp Web은 빌드 플래그 `whatsapp-web`이 필요합니다.
- Cloud와 Web 필드가 모두 있으면 하위 호환성을 위해 Cloud 모드가 우선합니다.

### `[channels_config.linq]`

iMessage, RCS, SMS를 위한 Linq Partner V3 API 통합입니다.

| 키 | 필수 | 용도 |
|---|---|---|
| `api_token` | 예 | Linq Partner API bearer 토큰 |
| `from_phone` | 예 | 발신 전화번호 (E.164 형식) |
| `signing_secret` | 선택 | HMAC-SHA256 서명 검증을 위한 webhook 서명 시크릿 |
| `allowed_senders` | 권장 | 허용된 인바운드 전화번호 (`[]` = 모두 거부, `"*"` = 모두 허용) |

참고:

- webhook endpoint는 `POST /linq`입니다.
- `ZEROCLAW_LINQ_SIGNING_SECRET`는 설정 시 `signing_secret`을 오버라이드합니다.
- 서명은 `X-Webhook-Signature`와 `X-Webhook-Timestamp` 헤더를 사용합니다. 오래된 타임스탬프(>300초)는 거부됩니다.
- 전체 config 예시는 [channels-reference.md](channels-reference.md)를 참조하십시오.

### `[channels_config.nextcloud_talk]`

네이티브 Nextcloud Talk 봇 통합 (webhook 수신 + OCS 전송 API)입니다.

| 키 | 필수 | 용도 |
|---|---|---|
| `base_url` | 예 | Nextcloud 기본 URL (예: `https://cloud.example.com`) |
| `app_token` | 예 | OCS bearer 인증에 사용되는 봇 앱 토큰 |
| `webhook_secret` | 선택 | webhook 서명 검증 활성화 |
| `allowed_users` | 권장 | 허용된 Nextcloud actor ID (`[]` = 모두 거부, `"*"` = 모두 허용) |
| `bot_name` | 선택 | Nextcloud Talk에서의 봇 표시 이름 (예: `"zeroclaw"`). 봇 자체 메시지를 필터링하고 피드백 루프를 방지하는 데 사용됩니다. |

참고:

- webhook endpoint는 `POST /nextcloud-talk`입니다.
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET`는 설정 시 `webhook_secret`을 오버라이드합니다.
- 설정 및 문제 해결은 [nextcloud-talk-setup.md](../../setup-guides/nextcloud-talk-setup.md)를 참조하십시오.

## `[hardware]`

물리적 접근(STM32, 프로브, 시리얼)을 위한 하드웨어 위저드 구성입니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 하드웨어 접근 활성화 여부 |
| `transport` | `none` | 전송 모드: `"none"`, `"native"`, `"serial"`, 또는 `"probe"` |
| `serial_port` | 미설정 | 시리얼 포트 경로 (예: `"/dev/ttyACM0"`) |
| `baud_rate` | `115200` | 시리얼 보드 레이트 |
| `probe_target` | 미설정 | 프로브 대상 칩 (예: `"STM32F401RE"`) |
| `workspace_datasheets` | `false` | 워크스페이스 데이터시트 RAG 활성화 (AI 핀 조회를 위한 PDF 회로도 인덱싱) |

참고:

- USB 시리얼 연결에는 `transport = "serial"`과 `serial_port`를 사용하십시오.
- 디버그 프로브 플래싱(예: ST-Link)에는 `transport = "probe"`와 `probe_target`을 사용하십시오.
- 프로토콜 세부 사항은 [hardware-peripherals-design.md](../../hardware/hardware-peripherals-design.md)를 참조하십시오.

## `[peripherals]`

상위 수준 주변 장치 보드 구성입니다. 활성화 시 보드가 agent 도구가 됩니다.

| 키 | 기본값 | 용도 |
|---|---|---|
| `enabled` | `false` | 주변 장치 지원 활성화 (보드가 agent 도구가 됨) |
| `boards` | `[]` | 보드 구성 목록 |
| `datasheet_dir` | 미설정 | RAG 검색을 위한 데이터시트 문서 경로 (워크스페이스 기준 상대 경로) |

`boards`의 각 항목:

| 키 | 기본값 | 용도 |
|---|---|---|
| `board` | _필수_ | 보드 유형: `"nucleo-f401re"`, `"rpi-gpio"`, `"esp32"` 등 |
| `transport` | `serial` | 전송: `"serial"`, `"native"`, `"websocket"` |
| `path` | 미설정 | 시리얼 경로: `"/dev/ttyACM0"`, `"/dev/ttyUSB0"` |
| `baud` | `115200` | 시리얼 보드 레이트 |

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets"

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"
```

참고:

- RAG 검색을 위해 보드 이름으로 명명된 `.md`/`.txt` 데이터시트 파일(예: `nucleo-f401re.md`, `rpi-gpio.md`)을 `datasheet_dir`에 배치하십시오.
- 보드 프로토콜 및 펌웨어 참고는 [hardware-peripherals-design.md](../../hardware/hardware-peripherals-design.md)를 참조하십시오.

## 보안 관련 기본값

- 기본 거부 channel allowlist (`[]`는 모두 거부를 의미)
- 기본적으로 gateway에서 페어링 필요
- 기본적으로 공용 바인드 비활성화

## 검증 명령어

config 편집 후:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
zeroclaw service restart
```

## 관련 문서

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](../../ops/operations-runbook.md)
- [troubleshooting.md](../../ops/troubleshooting.md)
