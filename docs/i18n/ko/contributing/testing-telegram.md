# 테스트 실행 가이드

## 빠른 참조

```bash
# 전체 자동화 테스트 스위트 (~2분)
./tests/telegram/test_telegram_integration.sh

# 빠른 스모크 테스트 (~10초)
./tests/telegram/quick_test.sh

# 컴파일 및 unit 테스트만 (~30초)
cargo test telegram --lib
```

## 생성된 항목

### 1. **test_telegram_integration.sh** (메인 테스트 스위트)

   - 모든 수정사항을 커버하는 **20개 이상의 자동화 테스트**
   - **6개 테스트 단계**: 코드 품질, 빌드, 설정, 헬스, 기능, 수동
   - 통과/실패 표시기가 있는 **컬러 출력**
   - 마지막에 **상세 요약**

   ```bash
   ./tests/telegram/test_telegram_integration.sh
   ```

### 2. **quick_test.sh** (빠른 검증)

   - 빠른 피드백을 위한 **4개 필수 테스트**
   - **10초 미만** 실행 시간
   - **pre-commit** 검사에 적합

   ```bash
   ./tests/telegram/quick_test.sh
   ```

### 3. **generate_test_messages.py** (테스트 도우미)

   - 다양한 길이의 테스트 메시지 생성
   - 메시지 분할 기능 테스트
   - 8가지 메시지 유형

   ```bash
   # 긴 메시지 생성 (>4096자)
   python3 tests/telegram/generate_test_messages.py long

   # 모든 메시지 유형 표시
   python3 tests/telegram/generate_test_messages.py all
   ```

### 4. **TESTING_TELEGRAM.md** (완전 가이드)

   - 포괄적인 테스트 문서
   - 문제 해결 가이드
   - 성능 벤치마크
   - CI/CD 통합 예제

## 단계별 가이드: 첫 실행

### 1단계: 자동화 테스트 실행

```bash
cd /Users/abdzsam/zeroclaw

# 스크립트를 실행 가능하게 만들기 (이미 완료)
chmod +x tests/telegram/test_telegram_integration.sh tests/telegram/quick_test.sh

# 전체 테스트 스위트 실행
./tests/telegram/test_telegram_integration.sh
```

**예상 출력:**
```
⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡

███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
...

🧪 TELEGRAM INTEGRATION TEST SUITE 🧪

Phase 1: Code Quality Tests
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Test 1: Compiling test suite
✓ PASS: Test suite compiles successfully

Test 2: Running Telegram unit tests
✓ PASS: All Telegram unit tests passed (24 tests)
...

Test Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total Tests:   20
Passed:        20
Failed:        0
Warnings:      0

Pass Rate:     100%

✓ ALL AUTOMATED TESTS PASSED! 🎉
```

### 2단계: Telegram 설정 (미완료 시)

```bash
# 안내형 설정
zeroclaw onboard

# 또는 채널만 설정
zeroclaw onboard --channels-only
```

프롬프트에서:
1. **Telegram** 채널 선택
2. @BotFather에서 받은 **봇 토큰** 입력
3. **Telegram 사용자 ID** 또는 사용자명 입력

### 3단계: 헬스 확인

```bash
zeroclaw channel doctor
```

**예상 출력:**
```
🩺 ZeroClaw Channel Doctor

  ✅ Telegram  healthy

Summary: 1 healthy, 0 unhealthy, 0 timed out
```

### 4단계: 수동 테스트

#### 테스트 1: 기본 메시지

```bash
# 터미널 1: 채널 시작
zeroclaw channel start
```

**Telegram에서:**
- 봇을 찾습니다
- 전송: `Hello bot!`
- **확인**: 봇이 3초 이내에 응답

#### 테스트 2: 긴 메시지 (분할 테스트)

```bash
# 긴 메시지 생성
python3 tests/telegram/generate_test_messages.py long
```

- **출력을 복사**
- **Telegram에서 봇에게 붙여넣기**
- **확인사항**:
  - 메시지가 2개 이상의 청크로 분할됨
  - 첫 번째 청크가 `(continues...)`로 끝남
  - 중간 청크에 `(continued)`와 `(continues...)`가 있음
  - 마지막 청크가 `(continued)`로 시작
  - 모든 청크가 순서대로 도착

#### 테스트 3: 단어 경계 분할

```bash
python3 tests/telegram/generate_test_messages.py word
```

- 봇에게 전송
- **확인**: 단어 경계에서 분할됨 (단어 중간이 아닌)

## 테스트 결과 체크리스트

모든 테스트를 실행한 후 확인합니다:

### 자동화 테스트

- [ ] 모든 20개 자동화 테스트 통과
- [ ] 빌드 성공적으로 완료
- [ ] 바이너리 크기 <10MB
- [ ] 헬스 체크 <5초에 완료
- [ ] clippy 경고 없음

### 수동 테스트

- [ ] 봇이 기본 메시지에 응답
- [ ] 긴 메시지가 올바르게 분할됨
- [ ] 계속 표시기가 나타남
- [ ] 단어 경계가 존중됨
- [ ] 허용 목록이 비인가 사용자를 차단
- [ ] 로그에 오류 없음

### 성능

- [ ] 응답 시간 <3초
- [ ] 메모리 사용량 <10MB
- [ ] 메시지 손실 없음
- [ ] 속도 제한 작동 (100ms 지연)

## 문제 해결

### 문제: 테스트 컴파일 실패

```bash
# 클린 빌드
cargo clean
cargo build --release

# 의존성 업데이트
cargo update
```

### 문제: "Bot token not configured"

```bash
# 설정 확인
cat ~/.zeroclaw/config.toml | grep -A 5 telegram

# 재설정
zeroclaw onboard --channels-only
```

### 문제: 헬스 체크 실패

```bash
# 봇 토큰 직접 테스트
curl "https://api.telegram.org/bot<YOUR_TOKEN>/getMe"

# 반환 예상: {"ok":true,"result":{...}}
```

### 문제: 봇이 응답하지 않음

```bash
# 디버그 로깅 활성화
RUST_LOG=debug zeroclaw channel start

# 다음을 확인:
# - "Telegram channel listening for messages..."
# - "ignoring message from unauthorized user" (허용 목록 문제인 경우)
# - 오류 메시지
```

## 성능 벤치마크

모든 수정 후 예상 결과:

| 지표 | 목표 | 명령 |
|--------|--------|---------|
| Unit 테스트 통과 | 24/24 | `cargo test telegram --lib` |
| 빌드 시간 | <30초 | `time cargo build --release` |
| 바이너리 크기 | ~3-4MB | `ls -lh target/release/zeroclaw` |
| 헬스 체크 | <5초 | `time zeroclaw channel doctor` |
| 첫 응답 | <3초 | Telegram에서 수동 테스트 |
| 메시지 분할 | <50ms | 디버그 로그 확인 |
| 메모리 사용량 | <10MB | `ps aux \| grep zeroclaw` |

## CI/CD 통합

워크플로우에 추가:

```bash
# Pre-commit hook
#!/bin/bash
./tests/telegram/quick_test.sh

# CI 파이프라인
./tests/telegram/test_telegram_integration.sh
```

## 다음 단계

1. **테스트를 실행합니다:**
   ```bash
   ./tests/telegram/test_telegram_integration.sh
   ```

2. 문제 해결 가이드를 사용하여 **실패를 수정합니다**

3. 체크리스트를 사용하여 **수동 테스트를 완료합니다**

4. 모든 테스트가 통과하면 **프로덕션에 배포합니다**

5. 문제가 없는지 **로그를 모니터링합니다**:
   ```bash
   zeroclaw daemon
   # 또는
   RUST_LOG=info zeroclaw channel start
   ```

## 성공!

모든 테스트가 통과하면:
- 메시지 분할이 작동합니다 (4096자 제한)
- 헬스 체크에 5초 타임아웃이 있습니다
- 빈 chat_id가 안전하게 처리됩니다
- 24개 unit 테스트 모두 통과
- 코드가 프로덕션 준비 완료

**Telegram 통합이 준비되었습니다!**

---

## 지원

- 이슈: https://github.com/zeroclaw-labs/zeroclaw/issues
- 문서: [testing-telegram.md](../../../../tests/manual/telegram/testing-telegram.md)
- 도움말: `zeroclaw --help`
