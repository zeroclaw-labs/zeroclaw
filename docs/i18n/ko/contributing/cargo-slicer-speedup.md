# cargo-slicer로 더 빠른 빌드

[cargo-slicer](https://github.com/nickel-org/cargo-slicer)는 MIR 수준에서 도달 불가능한 라이브러리 함수를 스텁 처리하여 최종 바이너리가 호출하지 않는 코드에 대한 LLVM 코드 생성을 건너뛰는 `RUSTC_WRAPPER`입니다.

## 벤치마크 결과

| 환경 | 모드 | 기준 | cargo-slicer 사용 | 벽시계 시간 절약 |
|---|---|---|---|---|
| 48코어 서버 | syn 사전 분석 | 3분 52초 | 3분 31초 | **-9.1%** |
| 48코어 서버 | MIR 정밀 | 3분 52초 | 2분 49초 | **-27.2%** |
| Raspberry Pi 4 | syn 사전 분석 | 25분 03초 | 17분 54초 | **-28.6%** |

모든 측정은 클린 `cargo +nightly build --release`입니다. MIR 정밀 모드는 실제 컴파일러 MIR을 읽어 더 정확한 호출 그래프를 구축하며, syn 기반 분석의 799개 대비 1,060개의 모노 아이템을 스텁 처리합니다.

## CI 통합

워크플로우 `.github/workflows/ci-build-fast.yml` (아직 구현되지 않음)은 표준 빌드와 함께 가속화된 릴리스 빌드를 실행하기 위한 것입니다. Rust 코드 변경 및 워크플로우 변경에서 트리거되며, merge를 게이트하지 않고, 비차단 검사로 병렬 실행됩니다.

CI는 탄력적인 2경로 전략을 사용합니다:
- **빠른 경로**: `cargo-slicer`와 `rustc-driver` 바이너리를 설치하고 MIR 정밀 슬라이스 빌드를 실행합니다.
- **폴백 경로**: `rustc-driver` 설치가 실패하면 (예: nightly `rustc` API 변동), 검사를 실패시키는 대신 일반 `cargo +nightly build --release`를 실행합니다.

이를 통해 툴체인이 호환될 때마다 가속을 유지하면서 검사를 유용하고 녹색으로 유지합니다.

## 로컬 사용법

```bash
# 일회성 설치
cargo install cargo-slicer
rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
cargo +nightly install cargo-slicer --profile release-rustc \
  --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
  --features rustc-driver

# syn 사전 분석으로 빌드 (zeroclaw 루트에서)
cargo-slicer pre-analyze
CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release

# MIR 정밀 분석으로 빌드 (더 많은 스텁, 더 큰 절약)
# 1단계: .mir-cache 생성 (MIR_PRECISE로 첫 빌드)
CARGO_SLICER_MIR_PRECISE=1 CARGO_SLICER_WORKSPACE_CRATES=zeroclaw,zeroclaw_robot_kit \
  CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release
# 2단계: 후속 빌드는 자동으로 .mir-cache 사용
```

## 작동 원리

1. **사전 분석**이 `syn`을 통해 워크스페이스 소스를 스캔하여 크로스 크레이트 호출 그래프를 구축합니다 (~2초).
2. **크로스 크레이트 BFS**가 `main()`에서 시작하여 실제로 도달 가능한 공개 라이브러리 함수를 식별합니다.
3. **MIR 스텁 처리**가 도달 불가능한 본문을 `Unreachable` 터미네이터로 교체합니다 — 모노 수집기가 호출 대상을 찾지 못하고 전체 코드 생성 하위 트리를 제거합니다.
4. **MIR 정밀 모드** (선택 사항)는 바이너리 크레이트 관점에서 실제 컴파일러 MIR을 읽어 더 많은 도달 불가능한 함수를 식별하는 근거 호출 그래프를 구축합니다.

소스 파일은 수정되지 않습니다. 출력 바이너리는 기능적으로 동일합니다.
