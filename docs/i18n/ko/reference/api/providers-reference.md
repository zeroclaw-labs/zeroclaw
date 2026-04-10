# ZeroClaw Provider 레퍼런스

이 문서는 provider ID, 별칭, 자격 증명 환경 변수를 매핑합니다.

최종 검증일: **2026년 3월 12일**.

## Provider 목록 조회 방법

```bash
zeroclaw providers
```

## 자격 증명 확인 순서

런타임 확인 순서:

1. config/CLI의 명시적 자격 증명
2. provider별 환경 변수
3. 범용 폴백 환경 변수: `ZEROCLAW_API_KEY` 다음 `API_KEY`

복원력 있는 폴백 체인(`reliability.fallback_providers`)의 경우, 각 폴백 provider는 독립적으로 자격 증명을 확인합니다. 기본 provider의 명시적 자격 증명은 폴백 provider에 재사용되지 않습니다.

## 폴백 Provider 체인

ZeroClaw는 기본 provider가 다음과 같은 상황을 만났을 때 대체 provider로의 자동 장애 조치를 지원합니다:

- 타임아웃 또는 연결 오류
- 서비스 불가(503)
- 속도 제한(429), API key 로테이션 소진 후
- 모델 미발견 오류 (모델별 폴백 구성 시)

`config.toml`에서 폴백 체인을 구성하십시오:

```toml
[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
provider_retries = 2
provider_backoff_ms = 500
```

동작:

1. 기본 provider 시도 (`provider_retries`와 지수 백오프 적용)
2. 일시적 장애 시, 첫 번째 폴백 provider로 이동
3. 각 폴백에 대해 순서대로 반복
4. 영구 오류(400, 401, 403)에서는 즉시 폴백으로 건너뜀

각 폴백 provider:
- 독립적으로 자격 증명을 확인합니다
- 다른 API 패밀리일 수 있습니다 (OpenAI 호환 -> Anthropic -> 로컬 Ollama)
- 가능하면 동일한 요청 모델을 재사용하거나, 구성된 경우 모델 폴백을 트리거합니다

예시: 멀티 클라우드 고가용성

```toml
default_provider = "openai"
default_model = "gpt-4o"

[reliability]
fallback_providers = ["anthropic", "ollama"]

[reliability.model_fallbacks]
"gpt-4o" = ["gpt-4-turbo"]
"claude-opus-4-20250514" = ["claude-sonnet-4-20250514"]
```

OpenAI 타임아웃 시:
1. 백오프와 함께 2회 재시도
2. Anthropic으로 폴백, `gpt-4o` 시도 (Anthropic이 동등한 모델 선택)
3. Anthropic 실패 시, 로컬 Ollama로 폴백
4. Ollama에 모델이 없으면, 모델 폴백 사용 (Sonnet)

### 속도 제한 시 API Key 로테이션

provider가 429(속도 제한)를 반환하면 ZeroClaw는:

1. `reliability.api_keys`의 다음 API key로 로테이션 (동일 provider/모델에서)
2. 모든 key가 소진되면, `fallback_providers`로 진행

추가 key 구성:

```toml
api_key = "sk-primary"  # 기본 key (항상 먼저 시도)

[reliability]
api_keys = ["sk-backup-1", "sk-backup-2"]  # 속도 제한 로테이션용 폴백 key
```

### 모델 폴백

특정 모델을 사용할 수 없거나 속도 제한된 경우, 모델별 폴백을 구성하십시오:

```toml
[reliability.model_fallbacks]
"gpt-4o" = ["gpt-4-turbo", "gpt-3.5-turbo"]
"claude-opus-4-20250514" = ["claude-sonnet-4-20250514"]
```

폴백이 트리거되는 경우:
- provider의 사용 가능한 모델에서 모델을 찾을 수 없음
- provider가 모델을 언급하는 오류 반환 (예: "model not found")
- 모델이 속도 제한되고 API key 로테이션이 소진됨

자세한 설정 지침은 [다중 모델 설정 및 폴백 체인](/docs/getting-started/multi-model-setup.md)을 참조하십시오.

## Provider 카탈로그

| 정식 ID | 별칭 | 로컬 | provider별 환경 변수 |
|---|---|---:|---|
| `openrouter` | — | 아니오 | `OPENROUTER_API_KEY` |
| `anthropic` | — | 아니오 | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| `openai` | — | 아니오 | `OPENAI_API_KEY` |
| `ollama` | — | 예 | `OLLAMA_API_KEY` (선택) |
| `gemini` | `google`, `google-gemini` | 아니오 | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |
| `venice` | — | 아니오 | `VENICE_API_KEY` |
| `vercel` | `vercel-ai` | 아니오 | `VERCEL_API_KEY` |
| `cloudflare` | `cloudflare-ai` | 아니오 | `CLOUDFLARE_API_KEY` |
| `moonshot` | `kimi` | 아니오 | `MOONSHOT_API_KEY` |
| `kimi-code` | `kimi_coding`, `kimi_for_coding` | 아니오 | `KIMI_CODE_API_KEY`, `MOONSHOT_API_KEY` |
| `synthetic` | — | 아니오 | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | 아니오 | `OPENCODE_API_KEY` |
| `opencode-go` | — | 아니오 | `OPENCODE_GO_API_KEY` |
| `zai` | `z.ai` | 아니오 | `ZAI_API_KEY` |
| `glm` | `zhipu` | 아니오 | `GLM_API_KEY` |
| `minimax` | `minimax-intl`, `minimax-io`, `minimax-global`, `minimax-cn`, `minimaxi`, `minimax-oauth`, `minimax-oauth-cn`, `minimax-portal`, `minimax-portal-cn` | 아니오 | `MINIMAX_OAUTH_TOKEN`, `MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | 아니오 | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (선택: `AWS_REGION`) |
| `qianfan` | `baidu` | 아니오 | `QIANFAN_API_KEY` |
| `doubao` | `volcengine`, `ark`, `doubao-cn` | 아니오 | `ARK_API_KEY`, `DOUBAO_API_KEY` |
| `qwen` | `dashscope`, `qwen-intl`, `dashscope-intl`, `qwen-us`, `dashscope-us`, `qwen-code`, `qwen-oauth`, `qwen_oauth` | 아니오 | `QWEN_OAUTH_TOKEN`, `DASHSCOPE_API_KEY` |
| `groq` | — | 아니오 | `GROQ_API_KEY` |
| `mistral` | — | 아니오 | `MISTRAL_API_KEY` |
| `xai` | `grok` | 아니오 | `XAI_API_KEY` |
| `deepseek` | — | 아니오 | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | 아니오 | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | 아니오 | `FIREWORKS_API_KEY` |
| `novita` | — | 아니오 | `NOVITA_API_KEY` |
| `perplexity` | — | 아니오 | `PERPLEXITY_API_KEY` |
| `cohere` | — | 아니오 | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | 아니오 | (config/`API_KEY` 폴백과 GitHub 토큰 사용) |
| `lmstudio` | `lm-studio` | 예 | (선택; 기본적으로 로컬) |
| `llamacpp` | `llama.cpp` | 예 | `LLAMACPP_API_KEY` (선택; 서버 인증 활성화 시에만) |
| `sglang` | — | 예 | `SGLANG_API_KEY` (선택) |
| `vllm` | — | 예 | `VLLM_API_KEY` (선택) |
| `osaurus` | — | 예 | `OSAURUS_API_KEY` (선택; 기본값 `"osaurus"`) |
| `nvidia` | `nvidia-nim`, `build.nvidia.com` | 아니오 | `NVIDIA_API_KEY` |
| `avian` | — | 아니오 | `AVIAN_API_KEY` |

### Vercel AI Gateway 참고

- Provider ID: `vercel` (별칭: `vercel-ai`)
- 기본 API URL: `https://ai-gateway.vercel.sh/v1`
- 인증: `VERCEL_API_KEY`
- Vercel AI Gateway 사용에는 프로젝트 배포가 필요하지 않습니다.
- `DEPLOYMENT_NOT_FOUND`가 표시되면, provider가 `https://api.vercel.ai` 대신 위의 gateway endpoint를 대상으로 하고 있는지 확인하십시오.

### Gemini 참고

- Provider ID: `gemini` (별칭: `google`, `google-gemini`)
- 인증은 `GEMINI_API_KEY`, `GOOGLE_API_KEY`, 또는 Gemini CLI OAuth 캐시(`~/.gemini/oauth_creds.json`)에서 가져올 수 있습니다
- API key 요청은 `generativelanguage.googleapis.com/v1beta`를 사용합니다
- Gemini CLI OAuth 요청은 Code Assist 요청 엔벨로프 시맨틱으로 `cloudcode-pa.googleapis.com/v1internal`을 사용합니다
- 사고 모델(예: `gemini-3-pro-preview`)이 지원됩니다 -- 내부 추론 파트는 응답에서 자동으로 필터링됩니다

### Ollama 비전 참고

- Provider ID: `ollama`
- 비전 입력은 사용자 메시지 이미지 마커를 통해 지원됩니다: ``[IMAGE:<source>]``.
- 멀티모달 정규화 후, ZeroClaw는 Ollama의 네이티브 `messages[].images` 필드를 통해 이미지 페이로드를 전송합니다.
- 비전을 지원하지 않는 provider가 선택된 경우, ZeroClaw는 이미지를 무시하는 대신 구조화된 기능 오류를 반환합니다.

### Ollama 클라우드 라우팅 참고

- `:cloud` 모델 접미사는 원격 Ollama endpoint에서만 사용하십시오.
- 원격 endpoint는 `api_url`에 설정해야 합니다(예: `https://ollama.com`).
- ZeroClaw는 `api_url`의 후행 `/api`를 자동으로 정규화합니다.
- `default_model`이 `:cloud`로 끝나는데 `api_url`이 로컬이거나 미설정이면, config 검증이 실행 가능한 오류 메시지와 함께 조기 실패합니다.
- 로컬 Ollama 모델 검색은 로컬 모드에서 클라우드 전용 모델 선택을 방지하기 위해 `:cloud` 항목을 의도적으로 제외합니다.

### llama.cpp 서버 참고

- Provider ID: `llamacpp` (별칭: `llama.cpp`)
- 기본 endpoint: `http://localhost:8080/v1`
- API key는 기본적으로 선택 사항입니다. `llama-server`가 `--api-key`로 시작된 경우에만 `LLAMACPP_API_KEY`를 설정하십시오.
- 모델 검색: `zeroclaw models refresh --provider llamacpp`

### SGLang 서버 참고

- Provider ID: `sglang`
- 기본 endpoint: `http://localhost:30000/v1`
- API key는 기본적으로 선택 사항입니다. 서버가 인증을 요구하는 경우에만 `SGLANG_API_KEY`를 설정하십시오.
- 도구 호출은 SGLang을 `--tool-call-parser`(예: `hermes`, `llama3`, `qwen25`)와 함께 실행해야 합니다.
- 모델 검색: `zeroclaw models refresh --provider sglang`

### vLLM 서버 참고

- Provider ID: `vllm`
- 기본 endpoint: `http://localhost:8000/v1`
- API key는 기본적으로 선택 사항입니다. 서버가 인증을 요구하는 경우에만 `VLLM_API_KEY`를 설정하십시오.
- 모델 검색: `zeroclaw models refresh --provider vllm`

### Osaurus 서버 참고

- Provider ID: `osaurus`
- 기본 endpoint: `http://localhost:1337/v1`
- API key 기본값은 `"osaurus"`이지만 선택 사항입니다. 오버라이드하려면 `OSAURUS_API_KEY`를 설정하거나, 키 없는 접근을 위해 미설정으로 두십시오.
- 모델 검색: `zeroclaw models refresh --provider osaurus`
- [Osaurus](https://github.com/dinoki-ai/osaurus)는 단일 endpoint를 통해 로컬 MLX 추론과 클라우드 provider 프록시를 결합하는 macOS(Apple Silicon)용 통합 AI 엣지 런타임입니다.
- 여러 API 형식을 동시에 지원합니다: OpenAI 호환(`/v1/chat/completions`), Anthropic(`/messages`), Ollama(`/chat`), Open Responses(`/v1/responses`).
- 도구 및 컨텍스트 서버 연결을 위한 내장 MCP (Model Context Protocol) 지원.
- 로컬 모델은 MLX(Llama, Qwen, Gemma, GLM, Phi, Nemotron 등)를 통해 실행되며, 클라우드 모델은 투명하게 프록시됩니다.

### Bedrock 참고

- Provider ID: `bedrock` (별칭: `aws-bedrock`)
- API: [Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- 인증: AWS AKSK (단일 API key가 아님). `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` 환경 변수를 설정하십시오.
- 선택 사항: 임시/STS 자격 증명용 `AWS_SESSION_TOKEN`, `AWS_REGION` 또는 `AWS_DEFAULT_REGION` (기본값: `us-east-1`).
- 기본 온보딩 모델: `anthropic.claude-sonnet-4-5-20250929-v1:0`
- 네이티브 도구 호출 및 프롬프트 캐싱(`cachePoint`) 지원.
- 교차 리전 추론 프로필 지원 (예: `us.anthropic.claude-*`).
- 모델 ID는 Bedrock 형식을 사용합니다: `anthropic.claude-sonnet-4-6`, `anthropic.claude-opus-4-6-v1` 등.

### Ollama 추론 토글

`config.toml`에서 Ollama 추론/사고 동작을 제어할 수 있습니다:

```toml
[runtime]
reasoning_enabled = false
```

동작:

- `false`: Ollama `/api/chat` 요청에 `think: false`를 전송합니다.
- `true`: `think: true`를 전송합니다.
- 미설정: `think`를 생략하고 Ollama/모델 기본값을 유지합니다.

### Kimi Code 참고

- Provider ID: `kimi-code`
- Endpoint: `https://api.kimi.com/coding/v1`
- 기본 온보딩 모델: `kimi-for-coding` (대안: `kimi-k2.5`)
- 런타임이 호환성을 위해 자동으로 `User-Agent: KimiCLI/0.77`을 추가합니다.

### NVIDIA NIM 참고

- 정식 provider ID: `nvidia`
- 별칭: `nvidia-nim`, `build.nvidia.com`
- 기본 API URL: `https://integrate.api.nvidia.com/v1`
- 모델 검색: `zeroclaw models refresh --provider nvidia`

권장 시작 모델 ID (2026년 2월 18일 NVIDIA API 카탈로그에서 검증됨):

- `meta/llama-3.3-70b-instruct`
- `deepseek-ai/deepseek-v3.2`
- `nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/llama-3.1-nemotron-ultra-253b-v1`

## 커스텀 Endpoint

- OpenAI 호환 endpoint:

```toml
default_provider = "custom:https://your-api.example.com"
```

- Anthropic 호환 endpoint:

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

## MiniMax OAuth 설정 (config.toml)

config에 MiniMax provider와 OAuth 플레이스홀더를 설정합니다:

```toml
default_provider = "minimax-oauth"
api_key = "minimax-oauth"
```

그런 다음 환경 변수로 다음 자격 증명 중 하나를 제공합니다:

- `MINIMAX_OAUTH_TOKEN` (선호, 직접 액세스 토큰)
- `MINIMAX_API_KEY` (레거시/정적 토큰)
- `MINIMAX_OAUTH_REFRESH_TOKEN` (시작 시 액세스 토큰 자동 갱신)

선택 사항:

- `MINIMAX_OAUTH_REGION=global` 또는 `cn` (provider 별칭에 따라 기본값)
- `MINIMAX_OAUTH_CLIENT_ID`: 기본 OAuth 클라이언트 ID 오버라이드

channel 호환성 참고:

- MiniMax 지원 channel 대화의 경우, 런타임 이력은 유효한 `user`/`assistant` 턴 순서를 유지하도록 정규화됩니다.
- channel별 전달 지침(예: Telegram 첨부 마커)은 후행 `system` 턴으로 추가되는 대신 선행 시스템 프롬프트에 병합됩니다.

## Qwen Code OAuth 설정 (config.toml)

config에 Qwen Code OAuth 모드를 설정합니다:

```toml
default_provider = "qwen-code"
api_key = "qwen-oauth"
```

`qwen-code`의 자격 증명 확인:

1. 명시적 `api_key` 값 (플레이스홀더 `qwen-oauth`가 아닌 경우)
2. `QWEN_OAUTH_TOKEN`
3. `~/.qwen/oauth_creds.json` (Qwen Code 캐시된 OAuth 자격 증명 재사용)
4. `QWEN_OAUTH_REFRESH_TOKEN`을 통한 선택적 갱신 (또는 캐시된 리프레시 토큰)
5. OAuth 플레이스홀더가 사용되지 않으면 `DASHSCOPE_API_KEY`가 여전히 폴백으로 사용 가능

선택적 endpoint 오버라이드:

- `QWEN_OAUTH_RESOURCE_URL` (필요 시 `https://.../v1`로 정규화됨)
- 미설정 시, 캐시된 OAuth 자격 증명의 `resource_url`이 가능할 때 사용됩니다

## 모델 라우팅 (`hint:<name>`)

`[[model_routes]]`를 사용하여 힌트로 모델 호출을 라우팅할 수 있습니다:

```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

그런 다음 힌트 모델 이름으로 호출합니다(예: 도구 또는 통합 경로에서):

```text
hint:reasoning
```

## 임베딩 라우팅 (`hint:<name>`)

동일한 힌트 패턴으로 `[[embedding_routes]]`를 사용하여 임베딩 호출을 라우팅할 수 있습니다.
`[memory].embedding_model`을 `hint:<name>` 값으로 설정하여 라우팅을 활성화합니다.

```toml
[memory]
embedding_model = "hint:semantic"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536

[[embedding_routes]]
hint = "archive"
provider = "custom:https://embed.example.com/v1"
model = "your-embedding-model-id"
dimensions = 1024
```

지원되는 임베딩 provider:

- `none`
- `openai`
- `custom:<url>` (OpenAI 호환 임베딩 endpoint)

선택적 라우트별 key 오버라이드:

```toml
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
api_key = "sk-route-specific"
```

## 안전한 모델 업그레이드

안정적인 힌트를 사용하고 provider가 모델 ID를 폐기할 때 라우트 대상만 업데이트하십시오.

권장 워크플로:

1. 호출 지점을 안정적으로 유지합니다(`hint:reasoning`, `hint:semantic`).
2. `[[model_routes]]` 또는 `[[embedding_routes]]` 아래의 대상 모델만 변경합니다.
3. 실행:
   - `zeroclaw doctor`
   - `zeroclaw status`
4. 롤아웃 전에 하나의 대표적인 흐름(채팅 + 메모리 검색)을 스모크 테스트합니다.

이렇게 하면 모델 ID가 업그레이드될 때 통합과 프롬프트를 변경할 필요가 없으므로 장애를 최소화합니다.
