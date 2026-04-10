# 테스트 가이드

ZeroClaw는 파일시스템 기반 구성의 5단계 테스트 분류 체계를 사용합니다.

## 테스트 분류 체계

| 레벨 | 테스트 대상 | 외부 경계 | 디렉토리 |
|-------|--------------|-------------------|-----------|
| **Unit** | 단일 함수/구조체 | 모두 모킹 | `#[cfg(test)]` 블록 (`src/**/*.rs`) 또는 별도의 `src/**/tests.rs` 파일 |
| **Component** | 자체 경계 내의 하나의 서브시스템 | 서브시스템은 실제, 나머지는 모킹 | `tests/component/` |
| **Integration** | 함께 연결된 여러 내부 컴포넌트 | 실제 내부, 외부 API는 모킹 | `tests/integration/` |
| **System** | 모든 내부 경계를 가로지르는 전체 요청->응답 | 외부 API만 모킹 | `tests/system/` |
| **Live** | 실제 외부 서비스와의 전체 스택 | 모킹 없음, `#[ignore]` | `tests/live/` |

## 디렉토리 구조

| 디렉토리 | 레벨 | 설명 | 실행 명령 |
|-----------|-------|-------------|-------------|
| `src/**/*.rs` | Unit | 소스와 함께 배치된 `#[cfg(test)]` 블록 또는 별도의 `tests.rs` 파일 | `cargo test --lib` |
| `tests/component/` | Component | 하나의 서브시스템, 실제 구현, 모킹된 경계 | `cargo test --test component` |
| `tests/integration/` | Integration | 함께 연결된 여러 컴포넌트 | `cargo test --test integration` |
| `tests/system/` | System | 전체 channel->agent->channel 흐름 | `cargo test --test system` |
| `tests/live/` | Live | 실제 외부 서비스, `#[ignore]` | `cargo test --test live -- --ignored` |
| `tests/manual/` | — | 사람이 주도하는 테스트 스크립트 (shell, Python) | 직접 실행 |
| `tests/support/` | — | 공유 모킹 인프라 (테스트 바이너리가 아님) | — |
| `tests/fixtures/` | — | 테스트 데이터 파일 (JSON 트레이스, 미디어) | — |

## 테스트 실행 방법

```bash
# 모든 테스트 실행 (unit + component + integration + system)
cargo test

# unit 테스트만 실행
cargo test --lib

# component 테스트 실행
cargo test --test component

# integration 테스트 실행
cargo test --test integration

# system 테스트 실행
cargo test --test system

# live 테스트 실행 (API 자격 증명 필요)
cargo test --test live -- --ignored

# 레벨 내에서 필터링
cargo test --test integration agent

# 전체 CI 검증
./dev/ci.sh all

# 레벨별 CI 명령
./dev/ci.sh test-component
./dev/ci.sh test-integration
./dev/ci.sh test-system
```

## 새 테스트 추가 방법

1. **하나의 서브시스템을 격리하여 테스트?** -> `tests/component/`
2. **여러 컴포넌트를 함께 테스트?** -> `tests/integration/`
3. **전체 메시지 흐름을 테스트?** -> `tests/system/`
4. **실제 API 키가 필요?** -> `tests/live/`에 `#[ignore]`와 함께

테스트 파일을 생성한 후 적절한 `mod.rs`에 추가하고 `tests/support/`의 공유 인프라를 사용합니다.

## 공유 인프라 (`tests/support/`)

모든 테스트 바이너리는 `mod support;`를 포함하여 `crate::support::*`를 통해 공유 모킹을 사용할 수 있습니다.

| 모듈 | 내용 |
|--------|----------|
| `mock_provider.rs` | `MockProvider` (FIFO 스크립트), `RecordingProvider` (요청 캡처), `TraceLlmProvider` (JSON 픽스처 재생) |
| `mock_tools.rs` | `EchoTool`, `CountingTool`, `FailingTool`, `RecordingTool` |
| `mock_channel.rs` | `TestChannel` (전송 캡처, 타이핑 이벤트 기록) |
| `helpers.rs` | `make_memory()`, `make_observer()`, `build_agent()`, `text_response()`, `tool_response()`, `StaticMemoryLoader` |
| `trace.rs` | `LlmTrace`, `TraceTurn`, `TraceStep` 타입 + `LlmTrace::from_file()` |
| `assertions.rs` | 선언적 트레이스 어설션을 위한 `verify_expects()` |

### 사용법

```rust
use crate::support::{MockProvider, EchoTool, CountingTool};
use crate::support::helpers::{build_agent, text_response, tool_response};
```

## JSON 트레이스 픽스처

트레이스 픽스처는 `tests/fixtures/traces/`에 JSON 파일로 저장된 미리 준비된 LLM 응답 스크립트입니다. 인라인 모킹 설정을 선언적 대화 스크립트로 대체합니다.

### 작동 방식

1. `TraceLlmProvider`가 픽스처를 로드하고 `Provider` trait을 구현합니다
2. 각 `provider.chat()` 호출은 FIFO 순서로 픽스처의 다음 단계를 반환합니다
3. 실제 도구가 정상적으로 실행됩니다 (예: `EchoTool`이 인자를 처리)
4. 모든 턴 후에 `verify_expects()`가 선언적 어설션을 검사합니다
5. 에이전트가 단계 수보다 더 많이 프로바이더를 호출하면 테스트가 실패합니다

### 픽스처 형식

```json
{
  "model_name": "test-name",
  "turns": [
    {
      "user_input": "User message",
      "steps": [
        {
          "response": {
            "type": "text",
            "content": "LLM response",
            "input_tokens": 20,
            "output_tokens": 10
          }
        }
      ]
    }
  ],
  "expects": {
    "response_contains": ["expected text"],
    "tools_used": ["echo"],
    "max_tool_calls": 1
  }
}
```

**응답 유형**: `"text"` (일반 텍스트) 또는 `"tool_calls"` (LLM이 도구 실행을 요청).

**expects 필드**: `response_contains`, `response_not_contains`, `tools_used`, `tools_not_used`, `max_tool_calls`, `all_tools_succeeded`, `response_matches` (정규식).

## Live 테스트 규칙

- 모든 live 테스트는 `#[ignore]`여야 합니다
- 자격 증명에 `env::var("ZEROCLAW_TEST_*")`를 사용합니다
- `cargo test --test live -- --ignored --nocapture`로 실행합니다

## 수동 테스트 (`tests/manual/`)

`cargo test`로 자동화할 수 없는 사람 주도 테스트용 스크립트:

| 디렉토리/파일 | 기능 |
|---|---|
| `manual/telegram/` | Telegram 통합 테스트 스위트, 스모크 테스트, 메시지 생성기 |
| `manual/test_dockerignore.sh` | `.dockerignore`가 민감한 경로를 제외하는지 검증 |

Telegram 관련 테스트 세부사항은 [testing-telegram.md](./testing-telegram.md)를 참조합니다.
