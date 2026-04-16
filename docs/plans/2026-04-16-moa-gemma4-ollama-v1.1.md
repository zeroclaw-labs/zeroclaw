# MoA × Gemma 4 × Ollama 통합 기술 명세서

**버전** v1.1 (2026-04-16) — 음성 채팅 및 동시통역 모듈 추가
**대상** MoA 앱 (ZeroClaw 포크 기반)
**목적** 이용자 하드웨어에 최적화된 Gemma 4 모델을 앱 설치 시 자동 다운로드하고, Ollama를 통해 기본 로컬 LLM으로 구동하여 (i) API 키 미입력 시 (ii) 오프라인 시 자동 fallback 경로로 활용한다.

---

## 1. 하드웨어-모델 매칭 매트릭스 (확정)

Gemma 4 4종 모델을 성능·메모리·라이선스 관점에서 분석한 결과, MoA는 4단계 하드웨어 티어로 분기한다. 모든 모델은 **Apache 2.0** 라이선스이고 Ollama 공식 레지스트리에 등재되어 있다.

| 티어 | 모델 | Ollama 태그 | 양자화 | 필요 메모리 (Q4_K_M) | 컨텍스트 | 멀티모달 | 대상 하드웨어 |
|------|------|-------------|--------|----------------------|----------|----------|---------------|
| **T1 Minimum** | Gemma 4 E2B (유효 2B / raw 5B) | `gemma4:e2b` | Q4_K_M | ~4 GB | 128K | 텍스트·이미지·**오디오** | 8GB RAM 모바일/저사양 노트북, Raspberry Pi 5, Jetson Orin Nano |
| **T2 Standard** | Gemma 4 E4B (유효 4B / raw 8B) | `gemma4:e4b` | Q4_K_M | ~5.5–6 GB | 128K | 텍스트·이미지·**오디오** | 16GB RAM 노트북, M1/M2 Air, 내장 GPU PC |
| **T3 High-perf** | Gemma 4 26B A4B (MoE, 활성 3.8–4B) | `gemma4:26b` | Q4_K_M | ~15 GB | 128K | 텍스트·이미지 | 16GB+ VRAM (RTX 4070 Ti 16GB, RTX 5060 Ti 16GB), M3 Pro 18GB+ 통합 메모리 |
| **T4 Workstation** | Gemma 4 31B Dense | `gemma4:31b` | Q4_K_M | ~17–20 GB (4K ctx) | 128K | 텍스트·이미지 | 24GB+ VRAM (RTX 4090, RTX 5090), M3/M4 Max 36GB+, H100 |

### 주요 판단 근거

E2B/E4B는 오디오 인코더(약 305M 파라미터, 40ms 프레임)를 네이티브로 포함하므로 변호사의 **음성 메모·의뢰인 면담 녹음 직접 처리**가 가능하다(특허 4호 청구항 7-2 실시 근거). 26B A4B는 MoE로 추론 시 4B만 활성화되어 **지연-민감 대화에 유리**하고, 31B Dense는 **장문 법률 문서 분석·복잡한 추론에 유리**하다. 장문 컨텍스트(128K 전체 활용) 시 31B는 40GB 이상 필요하므로 실무 기본값은 4K–32K로 제한한다.

### 보수적 다운그레이드 정책

탐지된 하드웨어가 경계선에 있으면 **한 티어 아래를 선택**한다. 예: 16GB VRAM이지만 OS·다른 앱이 상당량 점유하는 노트북은 T3 대신 T2를 설치한다. 사용자가 설정에서 수동 upgrade/downgrade는 언제든 가능하다.

---

## 2. 설치 시 자동 하드웨어 감지 및 모델 다운로드

### 2.1 감지 항목

MoA 설치 직후 **최초 실행 시 1회** 다음을 수집한다(이후 설정 화면 "하드웨어 재검사" 버튼으로 수동 재실행 가능).

- **OS·아키텍처**: Windows/macOS/Linux, x86_64/arm64
- **총 RAM / 가용 RAM**: 시스템 총 메모리와 현재 free 메모리
- **GPU 유무 및 VRAM**: NVIDIA(CUDA compute capability), AMD(ROCm), Apple Silicon 통합 메모리
- **CPU 코어 수 / AVX2·AVX-512 지원 여부**
- **디스크 여유 공간**: 최소 30GB 요구
- **네트워크 대역폭**: 초기 다운로드 시간 추정용

### 2.2 티어 결정 알고리즘 (의사코드)

```rust
fn detect_tier(hw: &Hardware) -> Tier {
    // 우선순위: dGPU VRAM > Apple 통합 메모리 > 시스템 RAM
    let effective_mem_gb = if hw.has_dedicated_gpu() {
        hw.gpu_vram_gb
    } else if hw.is_apple_silicon() {
        // Apple Silicon은 통합 메모리의 70%를 GPU에 할당 가능
        (hw.system_ram_gb as f32 * 0.70) as u32
    } else {
        // iGPU만 있는 경우 시스템 RAM - OS 오버헤드(4GB)
        hw.system_ram_gb.saturating_sub(4)
    };

    match effective_mem_gb {
        0..=5   => Tier::T1_E2B,       // ~4GB 필요
        6..=9   => Tier::T2_E4B,       // ~5.5–6GB 필요
        10..=19 => Tier::T3_26B_MoE,   // ~15GB 필요
        _       => Tier::T4_31B_Dense, // ~17–20GB 필요
    }
}
```

**모바일 앱(iOS/Android)은 무조건 T1 E2B 또는 T2 E4B로 제한**한다. 현재 아키텍처 상 31B Dense는 모바일 비대상이다.

### 2.3 다운로드 플로우

1. 티어 결정 후 사용자에게 **확인 다이얼로그** 표시: "당신의 기기는 T3 (Gemma 4 26B MoE, 약 15GB)에 적합합니다. 설치에는 약 8GB 다운로드가 필요합니다. 진행하시겠습니까?" (다운로드 크기는 Q4_K_M GGUF 기준 E2B ~2GB, E4B ~3GB, 26B ~8GB, 31B ~10GB)
2. 사용자 선택 옵션: (a) 권장 모델 설치 (기본), (b) 한 단계 낮은 모델 설치, (c) 나중에 설치 (채팅 시 BYOK/프록시만 사용)
3. Ollama가 미설치이면 먼저 Ollama 런타임을 설치(`ollama.com/download`). ZeroClaw에 Ollama가 이미 번들되어 있다면 해당 바이너리 활용.
4. `ollama pull gemma4:{e2b|e4b|26b|31b}` 실행. 진행률은 MoA UI에 스트리밍 표시.
5. 완료 시 `ollama list`로 검증 후 `moa_settings.toml`에 `default_local_model = "gemma4:e4b"` 기록.

### 2.4 다운로드 일시 중단·이어받기

대용량 모델 특성상 네트워크 단절 시 **이어받기**가 필수다. Ollama는 내부적으로 blob 청크 단위 재개를 지원하므로, MoA는 진행률 폴링만 수행하고 pull 실패 시 지수 백오프(exponential backoff)로 3회 재시도한다.

---

## 3. Ollama 기반 기본 채팅 파이프라인

### 3.1 라우팅 정책 (특허 1호 실시 구조 반영)

클라이언트 요청 수신 시 다음 순서로 경로를 결정한다.

```
if network_offline:
    → Ollama 로컬 Gemma 4 (강제)
elif user_privacy_mode == "strict":
    → Ollama 로컬 Gemma 4 (강제)
elif chat_mode == "앱채팅":
    if has_byok_key and user_setting == "quality_first":
        → BYOK 직결
    else:
        → Ollama 로컬 Gemma 4 (기본)
elif chat_mode == "채널채팅":
    if has_byok_key:
        → 프록시 토큰 중계 (서버 Zero-Storage)
    else:
        → Ollama 로컬 Gemma 4
elif chat_mode == "웹채팅":
    if has_byok_key:
        → BYOK 직결
    else:
        → Ollama 로컬 Gemma 4
```

**핵심 불변식**: API 키 미입력 또는 오프라인이면 **반드시** Ollama 로컬 Gemma 4로 서비스 연속성을 확보한다. 사용자가 "API 키를 입력하지 않아서 채팅이 안 된다"는 상황은 존재해서는 안 된다.

### 3.2 Ollama 호출 구현 (Rust Tauri 백엔드 가정)

```rust
use reqwest::Client;
use serde::{Serialize, Deserialize};

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    num_ctx: u32,        // 컨텍스트 길이
    temperature: f32,
    num_predict: i32,    // max tokens, -1 = unlimited
}

pub async fn chat_via_ollama(
    model: &str,
    messages: Vec<ChatMessage>,
    ctx_size: u32,
) -> Result<impl Stream<Item = Result<String>>> {
    let client = Client::new();
    let req = OllamaChatRequest {
        model: model.to_string(),
        messages,
        stream: true,
        options: OllamaOptions {
            num_ctx: ctx_size,
            temperature: 0.3, // 법률 도메인은 낮게
            num_predict: -1,
        },
    };
    let resp = client.post("http://127.0.0.1:11434/api/chat")
        .json(&req)
        .send()
        .await?;
    Ok(stream_sse_tokens(resp))
}
```

### 3.3 모델 상주 관리 (keep-alive)

Ollama는 기본 5분간 모델을 메모리에 상주시키고 이후 언로드한다. 변호사 업무처럼 짧은 상담이 드문드문 이어지는 패턴에서는 **매번 cold start(수 초 지연)**가 거슬린다. MoA는 앱 실행 중 `keep_alive: "30m"` 옵션을 부가하여 30분 상주시키고, 배터리 상태가 낮으면 `keep_alive: "0"`으로 즉시 언로드한다.

### 3.4 오프라인 감지

네트워크 상태는 OS API(Windows: `InternetGetConnectedState`, macOS: `SCNetworkReachability`, Linux: `nmcli`)와 **실제 endpoint ping**(`api.openai.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`)의 AND 조건으로 판정한다. DNS만 열려있고 실제 호출이 막힌 경우(방화벽·VPN)도 오프라인으로 처리한다.

---

## 4. 멀티모달(오디오·이미지) 분기

E2B/E4B는 오디오·이미지 모두 네이티브 처리 가능하지만 26B/31B는 **텍스트·이미지**만 처리한다. 따라서 사용자가 T3·T4 티어에 있으면서 음성 파일을 첨부한 경우, **오디오만 따로 E4B 경량 모델을 병용 다운로드**하는 하이브리드 구성을 지원한다.

```
T1/T2 사용자 → 단일 모델로 모든 모달리티 처리
T3/T4 사용자 → 메인 모델(26B/31B) + 보조 오디오 모델(E4B) 병용 다운로드 옵션
```

보조 다운로드는 사용자에게 "음성 첨부 기능을 사용하려면 E4B 모델(약 3GB)을 추가로 다운로드해야 합니다"라는 프롬프트를 통해 명시적 동의 후 진행한다.

---

## 5. 한국어·법률 도메인 품질 보정

Gemma 4는 다국어 사전학습이 강화되었으나 한국 법률 고유명사·판례 인용 품질은 여전히 BYOK 대형 모델 대비 열세다. MoA는 다음 보정 레이어를 적용한다.

첫째, **시스템 프롬프트 고정**에 "너는 한국 변호사를 보조하는 법률 AI이다. 모든 응답은 한국어로, 법령·판례 인용 시 정확한 번호·사건번호를 표기하고, 불확실한 경우 반드시 'BYOK 대형 모델에서 재확인 권장' 문구를 부가하라" 지시를 포함한다. 둘째, **RAG 우선 정책**: 로컬 Gemma 4가 답변 생성 전 반드시 MoA의 로컬 판례 DB·법령 DB를 먼저 검색하여 컨텍스트로 주입한다(특허 4호 Layer 3). 셋째, **신뢰도 배지**: UI에 "이 응답은 온디바이스 Gemma 4 E4B로 생성되었습니다. 중요한 법률 판단은 BYOK 대형 모델로 재확인을 권장합니다"를 명시하여 과신을 방지한다.

---

## 6. 사용자 설정 화면 (Settings UI)

설정 → LLM → "로컬 모델" 섹션에 다음 컨트롤을 배치한다.

- **현재 설치된 모델**: `gemma4:e4b` (Q4_K_M, 5.7GB) — 상태: 정상 · 마지막 사용: 3분 전
- **모델 변경**: 드롭다운으로 E2B/E4B/26B/31B 선택 (불가능한 티어는 회색 비활성화 + 툴팁 "VRAM 부족")
- **하드웨어 재검사**: 버튼, 눌러서 탐지 재실행
- **자동 업그레이드 알림**: 토글, 하드웨어 증설 시 "이제 더 큰 모델 설치 가능" 알림
- **오프라인 전용 모드**: 토글, 켜면 API 키가 있어도 무조건 로컬만 사용(strict 모드)
- **모델 언로드**: 버튼, 디스크에서 제거

---

## 7. Claude Code 구현 프롬프트 (PR 단위 분해)

### PR #1 — 하드웨어 탐지 모듈 (`src-tauri/src/hardware/`)

```
목적: MoA 최초 실행 시 사용자 하드웨어를 탐지하여 Gemma 4 티어를 결정.

구현:
1. `sysinfo` 크레이트로 OS·CPU·RAM 수집
2. NVIDIA GPU: `nvml-wrapper` 크레이트로 VRAM 조회
3. Apple Silicon: `sysctl hw.memsize` + Metal 장치 질의
4. AMD GPU: Linux는 `/sys/class/drm/*/device/mem_info_vram_total`, Windows는 DXGI
5. 위 매트릭스(§1)에 따라 Tier enum 반환
6. 결과를 `~/.moa/hardware_profile.json`에 영구 저장

테스트 케이스: 8GB/16GB/24GB/32GB, Apple M1/M2/M3, RTX 3060/4070/4090
완료 기준: 10개 이상 실장비에서 올바른 티어 반환 확인
```

### PR #2 — Ollama 설치·모델 다운로드 (`src-tauri/src/ollama/`)

```
목적: Ollama 런타임 존재 확인 → 없으면 설치 → Gemma 4 모델 pull.

구현:
1. `ollama --version` 실행으로 존재 확인
2. 없으면 OS별 공식 설치:
   - macOS: `curl -fsSL https://ollama.com/install.sh | sh`
   - Windows: MSI 다운로드 후 silent install
   - Linux: systemd 서비스로 등록
3. `ollama pull gemma4:{tier}` 서브프로세스 실행, stdout에서 진행률 파싱
4. Tauri event로 프론트엔드에 진행률 스트림
5. 완료 시 `ollama list` 파싱으로 검증

UI: 다운로드 진행 다이얼로그 (취소 버튼·이어받기 포함)
완료 기준: 티어별 모델 다운로드 후 `ollama run gemma4:e4b "안녕"` 성공
```

### PR #3 — 라우팅 엔진 개정 (`src-tauri/src/routing/`)

```
목적: 특허 1호 설계를 Gemma 4 기본 경로를 포함하여 구현.

구현:
1. `Router::route(request)` 함수에 §3.1 의사코드 적용
2. 네트워크 상태는 `network_monitor` 태스크가 5초 간격 polling
3. 오프라인 전환 시 사용자에게 토스트 "오프라인 감지, Gemma 4 로컬로 전환"
4. BYOK 키 없음 상태에서 라우팅 시 "API 키 없이 로컬 Gemma 4 사용 중" 배지 표시

테스트:
- Wi-Fi 끄기 → 로컬 fallback 동작 확인
- API 키 삭제 → 로컬 fallback 동작 확인
- 앱채팅 모드 → 로컬 강제 확인 (strict 모드)
```

### PR #4 — Ollama HTTP 클라이언트 (`src-tauri/src/llm/ollama_client.rs`)

```
목적: Ollama REST API(/api/chat, /api/generate)와 MoA를 연결.

구현:
1. `reqwest` 스트리밍 클라이언트, SSE 파싱
2. keep_alive 기본 "30m", 배터리 저 시 "0"
3. num_ctx는 채팅 모드별: 앱채팅 8K, 채널채팅 32K, 웹채팅 최대
4. temperature 기본 0.3 (법률), 사용자 설정으로 조정 가능
5. 멀티모달: 이미지는 base64 인코딩하여 `images` 필드, 오디오도 동일 (E2B/E4B 대상)

에러 처리:
- Ollama 미실행: 자동 시작 시도 3회 후 사용자 에러
- 모델 미로드: 첫 요청 시 20-60초 cold start 대기, 프로그레스 UI
```

### PR #5 — 설정 UI (`src/components/SettingsLocalModel.tsx`)

```
§6 컨트롤 구현. React + Tailwind.
드롭다운에서 티어 변경 시 기존 모델 rm 후 새 모델 pull 플로우 호출.
```

---

## 8. 특허 반영 체크리스트

본 명세가 출원 특허 권리범위 내에서 실시 가능한지 확인:

- **특허 1호 청구항 1**: Zero-Storage 중계 + 3계층 라우팅 → §3.1에서 실현 ✓
- **특허 1호 청구항 4**: 네트워크 단절 시 자동 SLM fallback → §3.4, §3.1에서 실현 ✓
- **특허 1호 청구항 8 (기능적)**: "개방형 라이선스 + 로컬 런타임 + 디바이스 주 메모리 적재 가능" → Gemma 4 Apache 2.0 + Ollama + 티어별 매핑으로 실현 ✓
- **특허 1호 청구항 8-2**: "메모리·성능에 따라 사전 등록된 복수 모델 중 자동 선택" → §2.2 알고리즘에서 실현 ✓
- **특허 4호 청구항 7-2 (오디오 네이티브)**: E2B/E4B 오디오 인코더 직접 처리 → §4에서 실현 ✓

---

## 9. 배포 체크리스트

- [ ] Ollama 번들 라이선스 확인 (MIT) → MoA 설치 패키지에 포함 가능
- [ ] Gemma 4 Apache 2.0 고지 문구를 MoA "라이선스 정보" 화면에 포함
- [ ] 최초 다운로드 시 통신사 데이터 요금 경고 (모바일)
- [ ] 디스크 여유 공간 30GB 미만 시 설치 중단 + 안내
- [ ] macOS 공증(notarization), Windows 코드 서명
- [ ] 하드웨어 프로파일 JSON은 로컬에만 저장, 어떤 경우에도 외부 송출 금지 (비밀유지 의무 정합성)

---

## 10. 운영 지표 (KPI)

로컬 라우팅 비율(API 비용 절감 효과), 평균 cold start 시간, 사용자 만족도(응답 품질 평가), 오프라인 fallback 발동 횟수를 **모두 로컬 SQLite에만** 기록하여 주간 리포트를 로컬에 생성한다. 원격 텔레메트리는 수집하지 않는다.

---

## 11. 음성 채팅 및 동시통역 모듈 (Voice Chat & Simultaneous Interpretation)

### 11.1 기존 스택과 교체·유지 원칙

MoA의 현행 음성 채팅은 **(i) Gemini 3.1 Flash Live API (end-to-end 오디오 생성) 경로**와 **(ii) Deepgram(STT) + Typecast(TTS) 경로**의 2개 온라인 스택이 이미 구현되어 있다. Gemini Live 경로는 end-to-end 오디오 모델의 감정 전달력과 최저 지연을 제공하고, Typecast 경로는 단순 음성 합성 도구가 아니라 두 가지 **독자적 제품 가치**를 담당한다.

첫째, 100개 이상의 음성을 연령·성별·직업·국적(억양 보존)별로 구분한 **"AI 비서 선택"** UX. 이는 단순 "목소리 고르기"가 아니라 사용자가 자신에게 맞는 AI 조수의 페르소나를 고르는 브랜드 차별화 요소이다.

둘째, **사용자 음성 클로닝 기반 동시통역**. 사용자가 한국어로 발화하면 짧은 지연 후 본인의 목소리 그대로 영어로 발화되는 기능으로, 경쟁 제품이 쉽게 모방하기 어려운 프리미엄 기능이다.

따라서 본 명세의 교체 원칙은 다음과 같다.

- **Gemini 3.1 Flash Live API(end-to-end 오디오)** → **유지**. 최상위 티어(Tier S)로 고정하여 최고 감정 전달력·최저 지연을 보장.
- **Deepgram(STT)** → Gemma 4 E4B의 네이티브 오디오 인코더로 **완전 대체**. 중간 텍스트화 손실이 제거되어 억양·침묵·강조 같은 음향 뉘앙스가 LLM 이해 단계까지 보존된다.
- **Typecast(TTS)** → **유지하되 4-티어 선택지**로 재구성. Typecast는 "온라인 프리미엄 비서/클로닝 경로"로 남기고, 오프라인·무료 사용자를 위한 오프라인 TTS 경로를 추가하여 서비스 연속성과 가격 유연성을 확보한다.

### 11.2 음성 스택 4-티어 정책

| 티어 | 엔진 | 아키텍처 | 대상 사용자 | 음성 수 | 감정 | 음성 클로닝 | 온라인 | 비용 |
|------|------|----------|-------------|---------|------|-------------|--------|------|
| **S (Live End-to-End)** | **Gemini 3.1 Flash Live API** (현행·구현완료) | end-to-end 오디오 생성 모델 (STT·LLM·TTS 일체화) | 최상급 품질·최저 지연을 원하는 온라인 사용자, 대화형·자연스러운 동시통역 선호 | 모델 제공 음성 | **end-to-end 감정 전달 최상** | API 정책에 따름 | 필수 | 유료 (Google 과금) |
| **A (Premium Online)** | **Typecast** (현행) | STT(Gemma 4 E4B) → LLM → Typecast TTS 3단 | 온라인 + 유료 구독자, **"AI 비서" 페르소나 선택** + **본인 음성 클로닝 동시통역** UX 선호 | 100+ (연령·성별·직업·국적 분류) | 스마트 감정 | ✓ (본인 클로닝 동시통역) | 필수 | 유료 |
| **B (Offline Pro)** | **CosyVoice 2** | STT(Gemma 4 E4B) → 로컬 LLM → CosyVoice 2 TTS 3단 | 오프라인에서도 본인 음성 클로닝 통역을 원하는 고사양 사용자 | 다수 (zero-shot) | 중상 | **✓ (로컬 zero-shot 클로닝)** | 불필요 | 무료 (Apache 2.0) |
| **C (Offline Basic)** | **Kokoro TTS** | STT(Gemma 4 E4B) → 로컬 LLM → Kokoro TTS 3단 | 무료·경량·일반 사용자 기본값 | ~50 (다국어) | 평탄 | ✗ | 불필요 | 무료 (Apache 2.0) |

**Tier B의 CosyVoice 2 채택 근거**: 한국어 포함 9개 언어 지원, 스트리밍 합성 지연 약 150ms, zero-shot 음성 클로닝(참조 음성 3-10초로 전사본 없이 클로닝) 네이티브 지원, Apache 2.0 라이선스. FunAudioLLM/CosyVoice 공식 레포지토리의 ONNX·PyTorch 추론 경로를 Rust Tauri 백엔드에서 사이드카 프로세스로 실행한다.

**대체 후보**: F5-TTS(한국어 품질 보통), IndexTTS-2(장문 우수, 한국어 보통), Fish Speech V1.5(빠르나 한국어 약함), GPT-SoVITS(한국어 양호, 파인튜닝 필요) 중 **CosyVoice 2가 "한국어 + 클로닝 + 스트리밍 + 라이선스"를 동시에 충족하는 유일 후보**로 판단됨. Tier B 엔진은 향후 Qwen3-TTS나 CosyVoice 3 출시 시 교체 가능하도록 추상화 인터페이스로 래핑한다.

### 11.3 "AI 비서 선택" UX 유지 전략

Tier A(Typecast) 사용자는 기존 100+ 비서 선택 UI가 그대로 유지된다. Tier B(CosyVoice 2)와 Tier C(Kokoro) 사용자를 위해서는 **"오프라인 비서 팩"**을 별도 구성한다. 즉 MoA가 Kokoro의 50여 음성과 CosyVoice 2로 생성한 샘플 음성 중 엄선된 10-20개에 한국어 닉네임·페르소나 카드(예: "박 변호사 비서", "김 팀장 조수", "영문 통역 담당 Emily" 등)를 부여하여 **오프라인에서도 비서 선택 경험 자체는 유지**되도록 한다. 음성 수는 줄지만 브랜드 UX는 보전된다.

사용자가 티어 A에서 B/C로 전환할 때, 평소 애용하던 Typecast 비서와 음색이 가장 유사한 오프라인 비서를 **자동 매핑 추천**하는 "비서 마이그레이션 도우미"를 제공한다.

### 11.4 동시통역 4-경로 설계

동시통역은 본 기능의 핵심 차별화 요소이므로 4-경로를 명시적으로 구분한다.

**경로 0 — Gemini 3.1 Flash Live API (Tier S, 구현완료)**. 사용자 한국어 입력 오디오를 그대로 Live API에 스트리밍 → end-to-end 모델이 이해·번역·영어 음성 생성을 일체로 수행 → 영어 오디오 스트림 출력. 품질 최상(감정·운율·휴지까지 자연 전달), 지연 최저(약 200-400ms). Google API 구독이 활성화된 온라인 사용자에게 기본값으로 제공.

**경로 1 — 온라인 + Typecast 구독 (Tier A, Premium)**. 기존 구조 유지. 사용자 음성 사전 클로닝 → 한국어 입력 → Gemma 4 E4B로 의도·감정 이해(종래 Deepgram 대비 개선) → 로컬에서 영어 번역 텍스트 생성(또는 온라인 LLM) → Typecast API에 클론 보이스 ID로 요청 → **사용자 본인 목소리**로 영어 출력. 품질 최상, 지연 약 300-500ms. **본인 음성 클로닝 통역은 Tier S가 제공하지 않는 Tier A 고유 기능**이므로, 사용자가 "내 목소리로 통역"을 선택한 경우 Tier S보다 Tier A가 우선한다.

**경로 2 — 오프라인 + 고사양 하드웨어 (Tier B, T3/T4) + 사용자가 클로닝 동의**. 사용자 음성 참조 샘플(3-10초)을 **로컬에만** 저장 → Gemma 4 E4B로 입력 이해 → 로컬 번역 → CosyVoice 2에 로컬 음성 참조 주입 → 사용자 본인 목소리 근사치로 영어 출력. 품질 상, 지연 약 500-800ms. **음성 참조는 ChaCha20-Poly1305 암호화로 로컬 저장하며 어떤 경우에도 외부 송출 금지**.

**경로 3 — 오프라인 + 일반 하드웨어 또는 클로닝 거부 (Tier C)**. Gemma 4 E4B 입력 이해 → 로컬 번역 → Kokoro의 사전 지정된 영어 비서 음성으로 출력. 품질 중, 지연 약 300-500ms. 본인 음성은 아니지만 오프라인·무료로 사용 가능.

### 11.5 라우팅 결정 트리 (음성 채팅 통합)

```
음성 입력 수신
 │
 ├─ [Tier S: Gemini 3.1 Flash Live API] 우선 분기
 │   조건: 온라인 + Google Live API 키 유효 + 사용자가 "본인 음성 클로닝"을
 │        명시적으로 선택하지 않음 + 사용자 설정이 Tier S 허용
 │   → Live API 양방향 오디오 스트림으로 직접 처리 (STT·LLM·TTS 일체화)
 │
 └─ [Tier S 조건 불충족 시] 3단 파이프라인 진입
     ├─ STT 단계: Gemma 4 E4B 네이티브 오디오 (Deepgram 대체)
     │   (E4B가 설치되지 않은 T3/T4 사용자는 §4 하이브리드 옵션에 따라 보조 E4B 병행 다운로드)
     │
     └─ TTS 단계:
         ├─ [동시통역 모드]
         │   ├─ 사용자가 "내 목소리로 통역" 선택 + 온라인 + Typecast 유효 → Tier A (Typecast 클로닝)
         │   ├─ 오프라인 + 사용자 클로닝 등록 + T3/T4 → Tier B (CosyVoice 2 클로닝)
         │   └─ 그 외 → Tier C (Kokoro 영어 비서)
         │
         └─ [일반 음성 채팅 모드]
             ├─ 온라인 + Typecast 유효 + 사용자가 "프리미엄 비서" 선택 → Tier A
             ├─ 오프라인 또는 Typecast 구독 없음 → Tier C (Kokoro, 기본값)
             └─ 사용자가 명시적으로 "오프라인 프로" 선택 → Tier B (CosyVoice 2)
```

**Tier S vs Tier A 우선순위 규칙**: 기본적으로 온라인·Live API 유효 시 Tier S가 우선한다. 다만 동시통역에서 **"본인 목소리 클로닝"** 기능을 사용자가 요청한 경우에는 Tier A(Typecast 클로닝)가 우선한다. 이는 Tier S의 end-to-end 모델이 현재 임의 화자 음성 클로닝을 제공하지 않기 때문이다. 즉 두 티어는 대체재가 아니라 **용도별 보완재**이다.

**핵심 불변식 (특허 1호 정합성)**: 오프라인이거나 Typecast 구독이 없는 상태에서도 **반드시 음성 채팅과 동시통역이 동작**한다. 온라인·결제 상태에 관계없는 서비스 연속성이 MoA의 차별화 포인트 중 하나이다.

### 11.6 감정·뉘앙스 보존 관점의 품질 위계

Tier S(Gemini 3.1 Flash Live API)는 end-to-end 오디오 생성 모델로 입력 감정을 직접 출력 감정에 반영하므로 정교한 감정 전달력이 우수하다(품질 위계 최상). 반면 Tier A/B/C의 경로는 "Gemma 4(입력단 감정 이해) → 텍스트 번역 → TTS 합성(출력단 감정 생성)"의 3단 파이프라인이므로 감정 전달이 한 단계 끊긴다. 이를 보완하기 위해 **감정 메타데이터 bridge**를 구현한다. 즉 Gemma 4 E4B가 입력 오디오를 이해할 때 생성하는 감정 태그(예: `emotion=concerned`, `intensity=high`, `register=formal`)를 구조화된 필드로 추출하여, TTS 호출 시 CosyVoice 2의 감정 컨트롤 프롬프트 또는 Typecast의 감정 파라미터로 전달한다. 완벽하지는 않으나 3단 파이프라인의 감정 단절을 상당 부분 복원한다.

### 11.7 하드웨어 요구 및 디스크 공간

Tier B(CosyVoice 2)는 약 1.5B 파라미터 규모이므로 추가 **4-5GB 디스크**, 추론 시 **6-8GB 메모리**를 요구한다. 따라서 §2 설치 감지 결과 T3 이상인 사용자에게만 기본 권장하고, T1/T2 사용자는 설정에서 수동 옵트인 가능. Tier C(Kokoro)는 82M 파라미터로 약 **300MB 디스크, 1GB 미만 메모리**로 전 티어에서 동작한다.

Kokoro는 모든 MoA 설치 시 **무조건 기본 설치**(오프라인 서비스 연속성 보장용 최저 안전망), CosyVoice 2는 **T3/T4 사용자에게 옵션 제공**, Typecast는 **기존 구독자의 API 키 유지 또는 신규 구독 유도**, Tier S는 **Google Live API 키 등록 시 자동 활성화**로 4-티어 체계가 완성된다.

### 11.8 구현 PR 추가

**PR #6 — Gemma 4 네이티브 오디오 STT 경로 (`src-tauri/src/voice/gemma_asr.rs`)**

Deepgram SDK 호출부를 Ollama `/api/chat` 멀티모달 엔드포인트의 오디오 입력 경로로 교체. 입력 오디오는 16kHz PCM 또는 OGG/Opus로 base64 인코딩하여 `audio` 필드 전달. 출력은 텍스트(전사) + Gemma 4의 감정 메타데이터 JSON. 전사 지연은 40ms 프레임 단위로 스트리밍.

**PR #7 — Kokoro TTS 번들 및 기본 음성 채팅 경로 (`src-tauri/src/voice/kokoro.rs`)**

Kokoro GGUF 모델과 ONNX Runtime 사이드카를 MoA 설치 패키지에 포함. Rust에서 `ort` 크레이트로 추론, PCM 스트리밍 출력. 기본 비서 10개(한국어 5 + 영어 5) 사전 지정, UI 카드 메타데이터와 매핑.

**PR #8 — CosyVoice 2 오프라인 클로닝 통역 (`src-tauri/src/voice/cosyvoice2.rs`)**

T3/T4 설치 시 옵션 다운로드. 사용자 음성 참조 등록 UI(3-10초 녹음 가이드). 클로닝 참조 벡터는 로컬 SQLite에 ChaCha20-Poly1305 암호화 저장. 동시통역 호출 시 참조 벡터 + 번역 텍스트 + 감정 메타데이터 주입.

**PR #9 — 음성 4-티어 라우터 (`src-tauri/src/voice/router.rs`)**

§11.5 결정 트리 구현. 기존에 구현된 Gemini 3.1 Flash Live API 클라이언트(Tier S)를 라우터 상위 분기로 편입하고, 그 아래에 Typecast/CosyVoice 2/Kokoro 3단 파이프라인 분기를 배치한다. Live API 키 유효성·Typecast API 헬스체크(5분 간격)·구독 만료·오프라인 전환을 감지하여 자동 티어 다운. UI에 현재 활성 티어 배지 표시("Gemini Live 사용 중" / "Typecast 프리미엄 사용 중" / "오프라인 프로 (CosyVoice 2)" / "오프라인 기본 (Kokoro)"). "내 목소리로 통역" 토글이 켜지면 Tier S를 건너뛰고 Tier A로 우선 라우팅.

**PR #10 — 비서 마이그레이션 도우미 (`src/components/SecretaryMigrator.tsx`)**

Typecast 비서 ID → 가장 유사한 오프라인 비서 매핑 테이블. 사용자 전환 시 "평소 쓰시던 '박 팀장' 비서와 음색이 가장 가까운 오프라인 비서는 '김 대리'입니다"와 같은 추천 UI.

### 11.9 특허 4호 추가 종속항 권고

본 통합 설계는 특허 4호(ACE) 청구항 7-2(네이티브 오디오 입력)를 넘어, **TTS 출력 경로에서도 독자적 구성**을 이루므로 다음 종속항 추가를 권고한다.

> 제7-2항에 있어서, 생성된 상기 구조화 요약 또는 번역 결과의 음성 출력 단계에서, (i) 외부 서버에서 실행되는 **종단간(end-to-end) 오디오 생성 모델 기반 경로**, (ii) 외부 서버에서 실행되는 **사용자 음성 클로닝 지원 프리미엄 합성 엔진 기반 경로**, (iii) 사용자 디바이스 내에서 실행되는 **zero-shot 음성 클로닝 합성 엔진 기반 경로**, (iv) 사용자 디바이스 내에서 실행되는 **경량 합성 엔진 기반 경로** 중 어느 하나를, 네트워크 상태·사용자 구독 상태·디바이스 사양·사용자 명시 설정(특히 "사용자 본인 음성에 의한 통역" 요청 여부)에 기초하여 자동 선택하되, 종단간 오디오 생성 모델 경로가 사용 불가능하거나 사용자 본인 음성 클로닝이 요청된 경우 음향 특징을 보존한 상태로 하위 경로에 감정 메타데이터를 전달하는 감정 메타데이터 브리지 모듈을 더 포함하는 것을 특징으로 하는, 적응형 컨텍스트 엔진.

본 종속항은 **특정 TTS 엔진에 구애받지 않는 기능적 한정**이므로 향후 더 우수한 TTS가 나와도 권리범위에 포섭된다.

---

## 부록 A. Ollama pull 실제 명령 (참조)

```bash
# T1 Minimum
ollama pull gemma4:e2b

# T2 Standard (권장 기본값)
ollama pull gemma4:e4b

# T3 High-perf (MoE, 저지연)
ollama pull gemma4:26b

# T4 Workstation
ollama pull gemma4:31b

# 대체: 양자화 명시
ollama pull gemma4:26b-a4b-it-q4_K_M

# 검증
ollama list
ollama run gemma4:e4b "대한민국 민법 제750조를 3문장으로 설명해줘"
```

## 부록 B. 모바일 번들 크기 최적화

iOS/Android의 경우 앱 번들에 직접 모델을 포함하지 않고 **최초 실행 시 on-demand 다운로드**하는 것이 App Store/Play Store 가이드라인에 부합한다(앱 크기 200MB 제한). Wi-Fi 연결 상태에서만 다운로드 허용.

---

**Sources (참고)**
- [Gemma 4 Hardware Requirements (Compute Market)](https://www.compute-market.com/blog/gemma-4-local-hardware-guide-2026)
- [Gemma 4 VRAM Requirements (Gemma 4 Guide)](https://gemma4guide.com/guides/gemma4-vram-requirements)
- [Unsloth Gemma 4 Run Locally](https://unsloth.ai/docs/models/gemma-4)
- [Ollama gemma4 library](https://ollama.com/library/gemma4)
- [Run Gemma with Ollama (Google)](https://ai.google.dev/gemma/docs/integrations/ollama)
- [What Gemma 4 Model Names Mean (BSWEN)](https://docs.bswen.com/blog/2026-04-03-gemma-4-model-variants-explained/)

---

## 구현 착수 전 검증 항목 (2026-04-16 기준)

본 기획서가 작성된 시점에 다음 가정이 **사후 검증 필요** 상태로 기록됨:

1. **Gemma 4 E2B/E4B 오디오 네이티브 입력** — ✅ **검증 완료 (2026-04-16T03:05 KST)**. `ollama show gemma4:e4b`에서 Capabilities에 `audio` 확인. 실제 테스트: 16kHz WAV 파일을 base64 인코딩 후 **`images` 필드**(NOT `audio` 필드)로 전달 시 "This is a test recording" 정확 전사 + 한국어 번역 성공. `prompt_eval_count` 29→106으로 오디오 토큰이 모델에 실제 도달 확인. **PR #6 구현 시 `images` 필드로 오디오 전달**, `audio` 필드는 Ollama 미지원.
2. **Tauri 경로 가정** — 본 기획서의 `src-tauri/src/...` 경로는 ZeroClaw 코어가 Tauri 백엔드임을 전제. 실제 리포지토리는 **Rust 코어(`src/`) + Tauri 프론트 래퍼(`clients/tauri/`)** 이원 구조이므로 PR #1~#4, #6~#9의 코어 로직은 `src/` 하위에, UI 바인딩만 `clients/tauri/src-tauri/`에 배치하도록 경로 매핑 필요.
3. **기존 구현 중복** — 탐색 결과 아래 항목은 이미 구현됨을 확인:
   - `src/providers/ollama.rs` (1,075줄) — PR #4의 HTTP 클라이언트 상당 부분 커버
   - `src/voice/gemini_live.rs`, `src/voice/simul_session.rs` — Tier S 동시통역 완전 구현
   - `src/voice/typecast_interp.rs`, `clients/tauri/src/components/VoicePicker.tsx`, `voice_match_score()` — Typecast 100+ 비서 선택·클로닝 UX 구현
   - `src/gateway/ws.rs:1494 handle_voice_socket()` — 음성 라우터 진입점
   - `src/hardware/` 는 peripherals(STM32/RPi GPIO)용이며 **호스트 하드웨어 감지용 아님**. PR #1은 신규 모듈(예: `src/host_probe/`) 필요.

이 세 가지 사항을 PR #1 착수 전에 (1)번을 먼저 검증하고, 결과에 따라 PR #6 상세 설계를 분기한다.
