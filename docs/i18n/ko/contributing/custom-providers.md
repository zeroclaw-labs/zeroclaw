# 커스텀 프로바이더 설정

ZeroClaw는 OpenAI 호환 및 Anthropic 호환 프로바이더 모두에 대해 커스텀 API 엔드포인트를 지원합니다.

## 프로바이더 유형

### OpenAI 호환 엔드포인트 (`custom:`)

OpenAI API 형식을 구현하는 서비스용:

```toml
default_provider = "custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

### Anthropic 호환 엔드포인트 (`anthropic-custom:`)

Anthropic API 형식을 구현하는 서비스용:

```toml
default_provider = "anthropic-custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

## 설정 방법

### 설정 파일

`~/.zeroclaw/config.toml`을 편집합니다:

```toml
api_key = "your-api-key"
default_provider = "anthropic-custom:https://api.example.com"
default_model = "claude-sonnet-4-6"
```

### 환경 변수

`custom:` 및 `anthropic-custom:` 프로바이더의 경우, 범용 키 환경 변수를 사용합니다:

```bash
export API_KEY="your-api-key"
# or: export ZEROCLAW_API_KEY="your-api-key"
zeroclaw agent
```

## llama.cpp 서버 (권장 로컬 설정)

ZeroClaw는 `llama-server`를 위한 일급 로컬 프로바이더를 포함합니다:

- 프로바이더 ID: `llamacpp` (별칭: `llama.cpp`)
- 기본 엔드포인트: `http://localhost:8080/v1`
- `llama-server`가 `--api-key`로 시작되지 않는 한 API 키는 선택 사항

로컬 서버 시작 (예시):

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

그 다음 ZeroClaw를 설정합니다:

```toml
default_provider = "llamacpp"
api_url = "http://127.0.0.1:8033/v1"
default_model = "ggml-org/gpt-oss-20b-GGUF"
default_temperature = 0.7
```

빠른 검증:

```bash
zeroclaw models refresh --provider llamacpp
zeroclaw agent -m "hello"
```

이 흐름에서는 `ZEROCLAW_API_KEY=dummy`를 내보낼 필요가 없습니다.

## SGLang 서버

ZeroClaw는 [SGLang](https://github.com/sgl-project/sglang)을 위한 일급 로컬 프로바이더를 포함합니다:

- 프로바이더 ID: `sglang`
- 기본 엔드포인트: `http://localhost:30000/v1`
- 서버가 인증을 요구하지 않는 한 API 키는 선택 사항

로컬 서버 시작 (예시):

```bash
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

그 다음 ZeroClaw를 설정합니다:

```toml
default_provider = "sglang"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

빠른 검증:

```bash
zeroclaw models refresh --provider sglang
zeroclaw agent -m "hello"
```

이 흐름에서는 `ZEROCLAW_API_KEY=dummy`를 내보낼 필요가 없습니다.

## vLLM 서버

ZeroClaw는 [vLLM](https://docs.vllm.ai/)을 위한 일급 로컬 프로바이더를 포함합니다:

- 프로바이더 ID: `vllm`
- 기본 엔드포인트: `http://localhost:8000/v1`
- 서버가 인증을 요구하지 않는 한 API 키는 선택 사항

로컬 서버 시작 (예시):

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

그 다음 ZeroClaw를 설정합니다:

```toml
default_provider = "vllm"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

빠른 검증:

```bash
zeroclaw models refresh --provider vllm
zeroclaw agent -m "hello"
```

이 흐름에서는 `ZEROCLAW_API_KEY=dummy`를 내보낼 필요가 없습니다.

## 설정 테스트

커스텀 엔드포인트를 검증합니다:

```bash
# 대화형 모드
zeroclaw agent

# 단일 메시지 테스트
zeroclaw agent -m "test message"
```

## 문제 해결

### 인증 오류

- API 키가 올바른지 확인합니다
- 엔드포인트 URL 형식을 확인합니다 (`http://` 또는 `https://`를 포함해야 합니다)
- 네트워크에서 엔드포인트에 접근 가능한지 확인합니다

### 모델을 찾을 수 없음

- 모델 이름이 프로바이더의 사용 가능한 모델과 일치하는지 확인합니다
- 정확한 모델 식별자는 프로바이더 문서를 확인합니다
- 엔드포인트와 모델 패밀리가 일치하는지 확인합니다. 일부 커스텀 게이트웨이는 모델의 하위 집합만 노출합니다.
- 설정한 동일한 엔드포인트와 키에서 사용 가능한 모델을 확인합니다:

```bash
curl -sS https://your-api.com/models \
  -H "Authorization: Bearer $API_KEY"
```

- 게이트웨이가 `/models`를 구현하지 않는 경우, 최소 chat 요청을 보내고 프로바이더가 반환한 모델 오류 텍스트를 확인합니다.

### 연결 문제

- 엔드포인트 접근성을 테스트합니다: `curl -I https://your-api.com`
- 방화벽/프록시 설정을 확인합니다
- 프로바이더 상태 페이지를 확인합니다

## 예시

### 로컬 LLM 서버 (범용 커스텀 엔드포인트)

```toml
default_provider = "custom:http://localhost:8080/v1"
api_key = "your-api-key-if-required"
default_model = "local-model"
```

### 기업 프록시

```toml
default_provider = "anthropic-custom:https://llm-proxy.corp.example.com"
api_key = "internal-token"
```

### 클라우드 프로바이더 게이트웨이

```toml
default_provider = "custom:https://gateway.cloud-provider.com/v1"
api_key = "gateway-api-key"
default_model = "gpt-4"
```
