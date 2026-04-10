# ZeroClaw 운영 런북

이 런북은 가용성, 보안 태세 및 인시던트 대응을 유지하는 운영자를 위한 문서입니다.

최종 검증일: **2026년 2월 18일**.

## 범위

이 문서는 Day-2 운영에 사용합니다:

- 런타임 시작 및 관리
- 상태 점검 및 진단
- 안전한 롤아웃 및 롤백
- 인시던트 분류 및 복구

최초 설치는 [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)에서 시작하십시오.

## 런타임 모드

| 모드 | 명령어 | 사용 시기 |
|---|---|---|
| 포그라운드 런타임 | `zeroclaw daemon` | 로컬 디버깅, 단기 세션 |
| 포그라운드 gateway 전용 | `zeroclaw gateway` | webhook 엔드포인트 테스트 |
| 사용자 서비스 | `zeroclaw service install && zeroclaw service start` | 지속적 운영자 관리 런타임 |
| Docker / Podman | `docker compose up -d` | 컨테이너화된 deployment |

## Docker / Podman 런타임

`./install.sh --docker`로 설치한 경우, 컨테이너는 온보딩 후 종료됩니다. ZeroClaw를
장기 실행 컨테이너로 운영하려면 리포지토리의 `docker-compose.yml`을 사용하거나
영구 데이터 디렉터리를 마운트하여 수동으로 컨테이너를 시작하십시오.

### 권장 방식: docker-compose

```bash
# 시작 (분리 모드, 재부팅 시 자동 재시작)
docker compose up -d

# 중지
docker compose down

# 재시작
docker compose up -d
```

Podman을 사용하는 경우 `docker`를 `podman`으로 교체하십시오.

### 수동 컨테이너 생명주기

```bash
# 부트스트랩 이미지에서 새 컨테이너 시작
docker run -d --name zeroclaw \
  --restart unless-stopped \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway

# 중지 (설정 및 workspace 보존)
docker stop zeroclaw

# 중지된 컨테이너 재시작
docker start zeroclaw

# 로그 확인
docker logs -f zeroclaw

# 상태 점검
docker exec zeroclaw zeroclaw status
```

Podman의 경우 `--userns keep-id --user "$(id -u):$(id -g)"`를 추가하고 볼륨 마운트에 `:Z`를 붙여주십시오.

### 핵심 주의사항: 재시작을 위해 install.sh를 다시 실행하지 마십시오

`install.sh --docker`를 다시 실행하면 이미지를 다시 빌드하고 온보딩을 재실행합니다. 단순히
재시작하려면 `docker start`, `docker compose up -d` 또는 `podman start`를 사용하십시오.

전체 설정 안내는 [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md#stopping-and-restarting-a-dockerpodman-container)를 참조하십시오.

## 기본 운영자 체크리스트

1. 설정 검증:

```bash
zeroclaw status
```

2. 진단 확인:

```bash
zeroclaw doctor
zeroclaw channel doctor
```

3. 런타임 시작:

```bash
zeroclaw daemon
```

4. 지속적 사용자 세션 서비스:

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

## 상태 및 신호

| 신호 | 명령어 / 파일 | 기대값 |
|---|---|---|
| 설정 유효성 | `zeroclaw doctor` | 치명적 오류 없음 |
| 채널 연결 상태 | `zeroclaw channel doctor` | 구성된 채널이 정상 |
| 런타임 요약 | `zeroclaw status` | 예상되는 provider/모델/채널 |
| 데몬 heartbeat/상태 | `~/.zeroclaw/daemon_state.json` | 파일이 주기적으로 갱신됨 |

## 로그 및 진단

### macOS / Windows (서비스 래퍼 로그)

- `~/.zeroclaw/logs/daemon.stdout.log`
- `~/.zeroclaw/logs/daemon.stderr.log`

### Linux (systemd 사용자 서비스)

```bash
journalctl --user -u zeroclaw.service -f
```

## 인시던트 분류 흐름 (빠른 경로)

1. 시스템 상태 스냅샷:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

2. 서비스 상태 확인:

```bash
zeroclaw service status
```

3. 서비스가 비정상이면 깨끗하게 재시작:

```bash
zeroclaw service stop
zeroclaw service start
```

4. 채널이 여전히 실패하면 `~/.zeroclaw/config.toml`의 허용 목록과 자격 증명을 확인하십시오.

5. gateway가 관련된 경우 바인드/인증 설정 (`[gateway]`) 및 로컬 접근 가능 여부를 확인하십시오.

## 안전한 변경 절차

설정 변경을 적용하기 전에:

1. `~/.zeroclaw/config.toml` 백업
2. 논리적 변경을 한 번에 하나씩 적용
3. `zeroclaw doctor` 실행
4. 데몬/서비스 재시작
5. `status` + `channel doctor`로 확인

## 롤백 절차

롤아웃 후 동작이 퇴행하는 경우:

1. 이전 `config.toml` 복원
2. 런타임 재시작 (`daemon` 또는 `service`)
3. `doctor` 및 채널 상태 점검으로 복구 확인
4. 인시던트 근본 원인 및 완화 조치 문서화

## 관련 문서

- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [troubleshooting.md](./troubleshooting.md)
- [config-reference.md](../reference/api/config-reference.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
