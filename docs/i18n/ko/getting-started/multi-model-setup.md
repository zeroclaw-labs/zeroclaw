# 멀티 모델 설정 및 Fallback 체인

이 가이드에서는 ZeroClaw의 멀티 모델 개념을 소개합니다. Fallback provider 체인, 모델 수준 fallback, 복원력을 위한 API 키 로테이션 등을 다룹니다.

최종 검증일: 2026년 3월 28일

## 멀티 모델 설정을 사용하는 경우

멀티 모델 구성은 다음과 같은 경우에 유용합니다:

- **높은 안정성**: 기본 provider가 실패하면 자동으로 대체 provider로 전환합니다
- **비용 최적화**: 속도 제한 시나리오에서 fallback 체인을 통해 비용이 높은 모델을 라우팅합니다
- **지역별 복원력**: 지리적으로 분산된 provider를 사용하여 특정 지역의 장애를 처리합니다
- **기능 유연성**: 필요한 기능(예: tool calling, vision)이 부족한 모델 대신 다른 모델을 시도합니다
- **속도 제한 처리**: `429`(속도 제한) 응답 시 API 키를 로테이션합니다
- **개발 및 테스트**: 코드 변경 없이 클라우드 모델과 로컬 모델 간 전환이 가능합니다

## 핵심 개념

### Fallback Provider 체인

provider에서 일시적인 오류(타임아웃, 연결 실패, 인증 문제)가 발생하면, ZeroClaw는 지정된 순서대로 자동으로 fallback provider를 시도합니다.

**예시**: 기본 provider가 `openai`이지만 일시적으로 사용할 수 없는 경우, ZeroClaw는 자동으로 `anthropic`으로 전환한 후 `groq`으로 전환할 수 있습니다.

```toml
[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
```

기본 provider가 복구되면 ZeroClaw는 다시 기본 provider를 사용합니다(고정 failover 없음).

### 모델 수준 Fallback

일부 모델은 모든 지역에서 사용할 수 없거나, 무거운 모델이 속도 제한에 걸렸을 때 더 빠른 모델을 사용하고 싶을 수 있습니다.

```toml
[reliability]
model_fallbacks = { "claude-opus-4-20250514" = ["claude-sonnet-4-20250514", "gpt-4o"] }
```

`claude-opus-4-20250514`가 실패하거나 사용할 수 없는 경우, ZeroClaw는 동일한 provider 내에서 fallback 모델을 순서대로 시도합니다(provider 수준 fallback도 함께 구성된 경우 제외).

### API 키 로테이션

속도 제한이 자주 발생하는 provider의 경우, `429` 응답 시 ZeroClaw가 로테이션할 추가 API 키를 제공할 수 있습니다.

```toml
[reliability]
api_keys = ["sk-key-2", "sk-key-3", "sk-key-4"]
```

기본 `api_key`(전역 또는 채널별로 구성된 키)가 항상 먼저 시도되며, 이 추가 키들은 속도 제한 오류 시 로테이션됩니다.

### Provider 재시도

각 provider 시도에는 다음 fallback으로 이동하기 전에 지수 백오프를 포함한 구성 가능한 재시도가 포함됩니다.

```toml
[reliability]
provider_retries = 2          # Provider당 재시도 횟수
provider_backoff_ms = 500     # 초기 백오프 시간(밀리초)
```

## 구성 구조

`config.toml`의 `[reliability]` 섹션:

| 키 | 타입 | 기본값 | 용도 |
|---|---|---|---|
| `fallback_providers` | `[string]` | `[]` | 순서가 지정된 fallback provider ID 목록 |
| `model_fallbacks` | `{string: [string]}` | `{}` | 모델 -> fallback 모델 목록 매핑 |
| `api_keys` | `[string]` | `[]` | 속도 제한 로테이션을 위한 추가 API 키 |
| `provider_retries` | `u32` | `2` | Failover 전 provider당 재시도 횟수 |
| `provider_backoff_ms` | `u64` | `500` | 초기 백오프 지연 시간(밀리초) |

## 구성 예시

### 기본 Fallback 체인

기본 provider에서 백업으로의 간단한 fallback을 설정합니다:

```toml
default_provider = "openai"
default_model = "gpt-4o"

[reliability]
fallback_providers = ["anthropic"]
```

**동작**: OpenAI가 타임아웃되거나 오류를 반환하면, ZeroClaw는 지수 백오프로 2번 재시도한 후 Anthropic을 사용하여 동일한 요청을 시도합니다.

### 고가용성 멀티 Provider 설정

Provider fallback, 모델 fallback, API 키 로테이션을 결합합니다:

```toml
default_provider = "openai"
default_model = "gpt-4o"
api_key = "sk-openai-primary"

[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
api_keys = ["sk-openai-backup-1", "sk-openai-backup-2"]

[reliability.model_fallbacks]
"gpt-4o" = ["gpt-4-turbo", "gpt-3.5-turbo"]
"gpt-4-turbo" = ["gpt-3.5-turbo"]
```

**동작**:
1. OpenAI `gpt-4o`를 기본 키로 시도합니다(2번 재시도)
2. 속도 제한 시 백업 API 키로 로테이션합니다
3. OpenAI가 여전히 실패하면 동일한 모델 요청으로 Anthropic으로 전환합니다(Anthropic이 사용 가능한 동등 모델을 선택)
4. Anthropic을 사용할 수 없으면 Groq, 그 다음 OpenRouter를 시도합니다
5. 모델을 사용할 수 없으면 fallback 모델을 순서대로 시도합니다

### 클라우드 Fallback을 갖춘 로컬 개발 환경

로컬 Ollama 인스턴스를 기본으로 사용하고 클라우드 provider로 fallback합니다:

```toml
default_provider = "ollama"
default_model = "llama2:70b"
api_url = "http://localhost:11434"

[reliability]
fallback_providers = ["openrouter", "groq"]
```

**동작**: Ollama가 다운되거나 타임아웃되면 구성 변경 없이 자동으로 OpenRouter 또는 Groq를 사용합니다.

### 비용 최적화: 고성능 모델과 빠른 Fallback

복잡한 작업에는 비용이 높은 reasoning 모델을 사용하되, 더 빠른 모델로 fallback합니다:

```toml
default_provider = "anthropic"
default_model = "claude-opus-4-20250514"

[reliability]
model_fallbacks = { "claude-opus-4-20250514" = ["claude-sonnet-4-20250514"] }
```

**동작**: Opus가 속도 제한에 걸리거나 느릴 때 자동으로 Sonnet을 사용합니다(일반적으로 2~3배 빠르고 저렴합니다).

## 멀티 리전 설정

멀티 리전 배포를 사용하는 조직의 경우:

```toml
# 기본 US 리전
default_provider = "anthropic"
default_model = "claude-sonnet-4-20250514"

[reliability]
# US Anthropic이 다운된 경우 EU 리전 provider로 fallback
fallback_providers = ["bedrock"]  # 여러 리전의 AWS Bedrock
provider_retries = 3
provider_backoff_ms = 1000
```

각 fallback provider에 대한 자격 증명이 환경에 설정되어 있는지 확인하세요:

```bash
export ANTHROPIC_API_KEY="..."
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
```

## Hot Reload 동작

`[reliability]` 섹션은 hot-reload가 가능합니다. 채널이나 게이트웨이가 실행 중인 상태에서 `config.toml`을 업데이트하면 재시작 없이 다음 수신 메시지부터 변경 사항이 적용됩니다.

업데이트 가능한 필드:
- `fallback_providers`
- `model_fallbacks`
- `api_keys`
- `provider_retries`
- `provider_backoff_ms`

## 오류 처리 및 Fallback 트리거

Fallback이 트리거되는 경우:

- **타임아웃**: 구성된 타임아웃 내에 provider가 응답하지 않은 경우
- **연결 오류**: 네트워크/DNS 장애
- **인증 오류**: 잘못된 자격 증명(일시적인 인증 서비스 문제가 감지된 경우에만 재시도)
- **속도 제한 (429)**: HTTP 429; API 키 로테이션을 먼저 시도한 후 provider fallback
- **서비스 불가 (503)**: 일시적인 서비스 문제
- **모델 미발견**: 구성된 경우 모델 fallback 체인을 트리거

Fallback이 트리거되지 **않는** 경우:

- **잘못된 요청 (400)**: 형식이 잘못된 입력; 재시도해도 도움이 되지 않음
- **영구적 인증 실패**: 잘못된 API 키 형식
- **모델 출력 오류**: 모델이 응답했지만 오류를 반환한 경우

## Fallback 활동 디버깅

Fallback 동작을 디버깅하려면 런타임 trace를 활성화하세요:

```toml
[observability]
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
```

그 다음 trace를 조회합니다:

```bash
# 모든 fallback 이벤트 표시
zeroclaw doctor traces --contains "fallback"

# Provider 재시도 세부 정보 표시
zeroclaw doctor traces --contains "provider"

# 속도 제한 로테이션 표시
zeroclaw doctor traces --contains "429"
```

## 모범 사례

1. **안정성 순서대로 정렬**: `fallback_providers`에 가장 안정적인 provider를 먼저 배치하세요
2. **Fallback 체인 테스트**: 프로덕션 사용 전에 fallback 동작을 확인하세요
3. **API 키 로테이션 모니터링**: 속도 제한 이벤트를 추적하여 로테이션이 활성화되는 시점을 파악하세요
4. **의미적으로 유사한 모델로 fallback 설정**: 의도 없이 reasoning 모델에서 chat 모델로 fallback하지 마세요
5. **환경 변수 사용**: 민감한 API 키는 config가 아닌 환경 변수에 저장하세요
6. **Fallback 의도 문서화**: config에 각 fallback이 존재하는 이유를 주석으로 추가하세요
7. **멀티 모델 자격 증명 확인**: 모든 fallback provider에 유효한 자격 증명이 설정되어 있는지 확인하세요

## 자격 증명 해결

각 fallback provider는 표준 해결 순서를 사용하여 독립적으로 자격 증명을 해결합니다:

1. config/CLI에서 명시적으로 지정된 자격 증명
2. Provider별 환경 변수
3. 일반 fallback: `ZEROCLAW_API_KEY`, 그 다음 `API_KEY`

**중요**: 기본 provider의 API 키는 fallback provider에 자동으로 재사용되지 않습니다. 각 provider에 대해 자격 증명을 별도로 설정하세요.

예시:

```bash
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="claude-..."
export GROQ_API_KEY="gsk-..."
```

## 제한 사항 및 제약 조건

- 최대 fallback provider 수: 구성 파일 크기에 의해 제한됨(일반적으로 100개 이상의 체인 지원)
- 모델당 최대 fallback 수: 하드 제한 없음
- API 키 로테이션: 타임아웃 전에 모든 키를 시도
- 재시도 횟수: 지수 백오프로 provider별 구성 가능
- 총 타임아웃 예산: 재시도 및 fallback 전체에 걸쳐 누적됨; 채널 수준 타임아웃이 여전히 적용됨

## 관련 문서

- [Config Reference: Reliability 섹션](/docs/reference/api/config-reference.md#reliability)
- [Providers Reference: Fallback Provider 체인](/docs/reference/api/providers-reference.md#fallback-provider-chains)
- [Observability 및 디버깅](/docs/ops/observability.md)
