# MCP 서버 등록

ZeroClaw는 **Model Context Protocol (MCP)**을 지원하여 외부 도구 및 컨텍스트 provider로 에이전트의 기능을 확장할 수 있습니다. 이 가이드에서는 MCP 서버를 등록하고 구성하는 방법을 설명합니다.

## 개요

MCP 서버는 세 가지 전송 유형으로 연결할 수 있습니다:
- **stdio**: 장시간 실행되는 로컬 프로세스 (예: Node.js 또는 Python 스크립트).
- **sse**: Server-Sent Events를 통한 원격 서버.
- **http**: 단순 HTTP POST 기반 서버.

## 구성

MCP 서버는 `config.toml`의 `[mcp]` 섹션에서 구성합니다.

```toml
[mcp]
enabled = true
deferred_loading = true # 권장: 필요할 때만 도구 스키마를 로드합니다

[[mcp.servers]]
name = "my_local_tool"
transport = "stdio"
command = "node"
args = ["/path/to/server.js"]
env = { "API_KEY" = "secret_value" }

[[mcp.servers]]
name = "my_remote_tool"
transport = "sse"
url = "https://mcp.example.com/sse"
```

### 서버 구성 필드

| 필드 | 타입 | 설명 |
|-------|------|-------------|
| `name` | String | **필수**. 도구 접두사로 사용되는 표시 이름입니다 (`name__tool_name`). |
| `transport` | String | `stdio`, `sse`, 또는 `http`. 기본값: `stdio`. |
| `command` | String | (stdio 전용) 실행할 실행 파일입니다. |
| `args` | List | (stdio 전용) 명령줄 인수입니다. |
| `env` | Map | (stdio 전용) 환경 변수입니다. |
| `url` | String | (sse/http 전용) 서버 엔드포인트 URL입니다. |
| `headers` | Map | (sse/http 전용) 사용자 정의 HTTP 헤더입니다 (예: 인증용). |
| `tool_timeout_secs` | Integer | 이 서버의 도구에 대한 호출별 타임아웃입니다. |

## 보안 및 자동 승인

기본적으로, 자율성 수준이 `full`로 설정되지 않은 한 MCP 서버의 모든 도구 실행은 수동 승인이 필요합니다.

특정 MCP 서버의 도구를 자동으로 승인하려면, `[autonomy]` 섹션의 `auto_approve` 목록에 해당 접두사를 추가하세요:

```toml
[autonomy]
auto_approve = [
  "my_local_tool__read_file", # 'my_local_tool'의 특정 도구 허용
  "my_remote_tool__get_weather" # 'my_remote_tool'의 특정 도구 허용
]
```

## 팁

- **도구 필터링**: 프로젝트 구성에서 `tool_filter_groups`를 사용하여 LLM에 노출되는 MCP 도구를 제한할 수 있습니다.
- **지연 로딩**: `deferred_loading = true`를 유지하면 도구 이름만 LLM에 전송하여 초기 토큰 오버헤드를 줄입니다. 에이전트가 도구를 사용하기로 결정할 때만 전체 스키마를 가져옵니다.
