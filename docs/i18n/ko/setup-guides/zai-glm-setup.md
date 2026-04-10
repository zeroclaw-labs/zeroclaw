# Z.AI GLM 설정

ZeroClaw는 OpenAI 호환 엔드포인트를 통해 Z.AI의 GLM 모델을 지원합니다.
이 가이드에서는 현재 ZeroClaw provider 동작에 맞는 실용적인 설정 옵션을 다룹니다.

## 개요

ZeroClaw는 다음 Z.AI 별칭과 엔드포인트를 기본적으로 지원합니다:

| 별칭 | 엔드포인트 | 참고 |
|-------|----------|-------|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | 글로벌 엔드포인트 |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | 중국 엔드포인트 |

사용자 정의 기본 URL이 필요한 경우 [`../contributing/custom-providers.md`](../contributing/custom-providers.md)를 참조하세요.

## 설정

### 빠른 시작

```bash
zeroclaw onboard \
  --provider "zai" \
  --api-key "YOUR_ZAI_API_KEY"
```

### 수동 구성

`~/.zeroclaw/config.toml`을 편집하세요:

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "zai"
default_model = "glm-5"
default_temperature = 0.7
```

## 사용 가능한 모델

| 모델 | 설명 |
|-------|-------------|
| `glm-5` | 온보딩 기본값; 가장 강력한 추론 |
| `glm-4.7` | 강력한 범용 품질 |
| `glm-4.6` | 균형 잡힌 기본 모델 |
| `glm-4.5-air` | 낮은 지연 시간 옵션 |

모델 가용성은 계정/지역에 따라 다를 수 있으므로, 확실하지 않을 때는 `/models` API를 사용하세요.

## 설정 검증

### curl로 테스트

```bash
# OpenAI 호환 엔드포인트 테스트
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

예상 응답:
```json
{
  "choices": [{
    "message": {
      "content": "Hello! How can I help you today?",
      "role": "assistant"
    }
  }]
}
```

### ZeroClaw CLI로 테스트

```bash
# 에이전트 직접 테스트
echo "Hello" | zeroclaw agent

# 상태 확인
zeroclaw status
```

## 환경 변수

`.env` 파일에 추가하세요:

```bash
# Z.AI API Key
ZAI_API_KEY=your-id.secret

# 선택적 범용 키 (많은 provider에서 사용)
# API_KEY=your-id.secret
```

키 형식은 `id.secret`입니다 (예: `abc123.xyz789`).

## 문제 해결

### 속도 제한

**증상:** `rate_limited` 오류

**해결 방법:**
- 대기 후 재시도
- Z.AI 플랜 제한 확인
- 낮은 지연 시간과 높은 할당량 허용을 위해 `glm-4.5-air` 시도

### 인증 오류

**증상:** 401 또는 403 오류

**해결 방법:**
- API 키 형식이 `id.secret`인지 확인
- 키가 만료되지 않았는지 확인
- 키에 불필요한 공백이 없는지 확인

### 모델을 찾을 수 없음

**증상:** 모델을 사용할 수 없다는 오류

**해결 방법:**
- 사용 가능한 모델 목록 조회:
```bash
curl -s "https://api.z.ai/api/coding/paas/v4/models" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" | jq '.data[].id'
```

## API 키 받기

1. [Z.AI](https://z.ai)로 이동합니다
2. Coding Plan에 가입합니다
3. 대시보드에서 API 키를 생성합니다
4. 키 형식: `id.secret` (예: `abc123.xyz789`)

## 관련 문서

- [ZeroClaw README](../README.md)
- [Custom Provider Endpoints](../contributing/custom-providers.md)
- [Contributing Guide](../../CONTRIBUTING.md)
