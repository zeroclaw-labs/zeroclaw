# SOP 관찰성 및 감사

이 페이지는 SOP 실행 증거가 저장되는 위치와 조회 방법을 다룹니다.

## 1. 감사 영속성

SOP 감사 항목은 `SopAuditLogger`를 통해 구성된 Memory 백엔드에 카테고리 `sop`로 저장됩니다.

일반적인 키 패턴:

- `sop_run_{run_id}`: 실행 스냅샷 (시작 + 완료 업데이트)
- `sop_step_{run_id}_{step_number}`: 단계별 결과
- `sop_approval_{run_id}_{step_number}`: 운영자 승인 기록
- `sop_timeout_approve_{run_id}_{step_number}`: 타임아웃 자동 승인 기록

## 2. 조회 경로

### 2.1 정의 수준 CLI

```bash
zeroclaw sop list
zeroclaw sop validate [name]
zeroclaw sop show <name>
```

### 2.2 런타임 실행 상태 도구

SOP 실행 상태는 agent 내 도구에서 조회합니다:

- `sop_status` -- 활성/완료된 실행 및 선택적 메트릭
- `sop_status` + `include_gate_status: true` -- 신뢰 단계 및 게이트 평가기 상태 (가능한 경우)
- `sop_approve` -- 대기 중인 실행 단계 승인
- `sop_advance` -- 단계 결과 제출 및 실행 진행

## 3. 메트릭

- `/metrics`는 `[observability] backend = "prometheus"`일 때 옵저버 메트릭을 노출합니다.
- 현재 내보내는 이름은 `zeroclaw_*` 패밀리(일반 런타임 메트릭)입니다.
- SOP 관련 집계는 `sop_status` + `include_metrics: true`를 통해 사용할 수 있습니다.
