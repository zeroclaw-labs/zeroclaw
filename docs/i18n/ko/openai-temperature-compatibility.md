# OpenAI Temperature 호환성 레퍼런스

이 문서는 OpenAI 모델 간 temperature 파라미터 호환성에 대한 실증적 증거를 제공합니다.

## 요약

OpenAI 모델 패밀리마다 서로 다른 temperature 요구 사항이 있습니다:

- **Reasoning 모델** (o 시리즈, gpt-5 기본 변형): `temperature=1.0`만 허용합니다
- **Search 모델**: temperature 파라미터를 허용하지 않습니다(생략해야 합니다)
- **표준 모델** (gpt-3.5, gpt-4, gpt-4o): 유연한 temperature 값(0.0-2.0)을 허용합니다

## 테스트된 모델

### temperature=1.0이 필요한 모델

| 모델 | 0.7 허용 | 1.0 허용 | 권장 사항 |
|-------|-------------|-------------|----------------|
| o1 | ❌ | ✅ | USE_1.0 |
| o1-2024-12-17 | ❌ | ✅ | USE_1.0 |
| o3 | ❌ | ✅ | USE_1.0 |
| o3-2025-04-16 | ❌ | ✅ | USE_1.0 |
| o3-mini | ❌ | ✅ | USE_1.0 |
| o3-mini-2025-01-31 | ❌ | ✅ | USE_1.0 |
| o4-mini | ❌ | ✅ | USE_1.0 |
| o4-mini-2025-04-16 | ❌ | ✅ | USE_1.0 |
| gpt-5 | ❌ | ✅ | USE_1.0 |
| gpt-5-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5-mini | ❌ | ✅ | USE_1.0 |
| gpt-5-mini-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5-nano | ❌ | ✅ | USE_1.0 |
| gpt-5-nano-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5.1-chat-latest | ❌ | ✅ | USE_1.0 |
| gpt-5.2-chat-latest | ❌ | ✅ | USE_1.0 |
| gpt-5.3-chat-latest | ❌ | ✅ | USE_1.0 |

### 유연한 Temperature를 허용하는 모델 (0.7 사용 가능)

모든 표준 GPT 모델은 유연한 temperature 값을 허용합니다:
- gpt-3.5-turbo (모든 변형)
- gpt-4 (모든 변형)
- gpt-4-turbo (모든 변형)
- gpt-4o (모든 변형)
- gpt-4o-mini (모든 변형)
- gpt-4.1 (모든 변형)
- gpt-5-chat-latest
- gpt-5.2, gpt-5.2-2025-12-11
- gpt-5.4, gpt-5.4-2026-03-05

### Temperature 생략이 필요한 모델

Search-preview 모델은 temperature 파라미터를 허용하지 않습니다:
- gpt-4o-mini-search-preview
- gpt-4o-search-preview
- gpt-5-search-api

## 구현

`src/providers/openai.rs`의 `adjust_temperature_for_model()` 함수가 reasoning 모델에 대해 자동으로 temperature를 1.0으로 조정하며, 표준 모델에 대해서는 사용자가 지정한 값을 유지합니다.

## 테스트 방법론

모델은 다음 조건으로 테스트되었습니다:
1. temperature 파라미터 없음 (기준선)
2. temperature=0.7 (일반적인 기본값)
3. temperature=1.0 (reasoning 모델 요구 사항)

결과는 실제 OpenAI API 응답과 대조하여 검증되었습니다.

## 참고 자료

- OpenAI API 문서: https://platform.openai.com/docs/api-reference/chat
- 관련 이슈: o1/o3/gpt-5 모델의 temperature 오류
