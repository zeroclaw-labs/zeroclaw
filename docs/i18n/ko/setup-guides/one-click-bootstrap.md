# 원클릭 부트스트랩

이 페이지는 ZeroClaw를 설치하고 초기화하는 가장 빠른 지원 경로를 정의합니다.

최종 검증일: **2026년 2월 20일**.

## 옵션 0: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

## 옵션 A (권장): Clone + 로컬 스크립트

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

기본적으로 수행하는 작업:

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### 리소스 사전 검사 및 사전 빌드 흐름

소스 빌드에는 일반적으로 최소한 다음이 필요합니다:

- **2 GB RAM + swap**
- **6 GB 여유 디스크**

리소스가 제한된 경우, 부트스트랩은 먼저 사전 빌드된 바이너리를 시도합니다.

```bash
./install.sh --prefer-prebuilt
```

바이너리 전용 설치를 요구하고 호환되는 릴리스 에셋이 없으면 실패하도록 하려면:

```bash
./install.sh --prebuilt-only
```

사전 빌드 흐름을 우회하고 소스 컴파일을 강제하려면:

```bash
./install.sh --force-source-build
```

## 듀얼 모드 부트스트랩

기본 동작은 **앱 전용** (ZeroClaw 빌드/설치)이며 기존 Rust 툴체인이 있어야 합니다.

새 머신의 경우, 환경 부트스트랩을 명시적으로 활성화하세요:

```bash
./install.sh --install-system-deps --install-rust
```

참고:

- `--install-system-deps`는 컴파일러/빌드 사전 요구사항을 설치합니다 (`sudo`가 필요할 수 있습니다).
- `--install-rust`는 Rust가 없을 때 `rustup`을 통해 설치합니다.
- `--prefer-prebuilt`는 먼저 릴리스 바이너리 다운로드를 시도한 후, 소스 빌드로 폴백합니다.
- `--prebuilt-only`는 소스 폴백을 비활성화합니다.
- `--force-source-build`는 사전 빌드 흐름을 완전히 비활성화합니다.

## 옵션 B: 원격 원라이너

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

보안이 중요한 환경에서는 스크립트를 실행 전에 검토할 수 있도록 옵션 A를 권장합니다.

저장소 체크아웃 외부에서 옵션 B를 실행하면, 설치 스크립트가 자동으로 임시 workspace를 클론하고, 빌드하고, 설치한 다음 정리합니다.

## 선택적 온보딩 모드

### 컨테이너 온보딩 (Docker)

```bash
./install.sh --docker
```

이 명령은 로컬 ZeroClaw 이미지를 빌드하고 컨테이너 내에서 온보딩을 시작하면서
config/workspace를 `./.zeroclaw-docker`에 유지합니다.

컨테이너 CLI는 기본적으로 `docker`입니다. Docker CLI를 사용할 수 없고 `podman`이 있으면,
설치 프로그램이 자동으로 `podman`으로 폴백합니다. `ZEROCLAW_CONTAINER_CLI`를 명시적으로
설정할 수도 있습니다 (예: `ZEROCLAW_CONTAINER_CLI=podman ./install.sh --docker`).

Podman의 경우, 설치 프로그램은 `--userns keep-id`와 `:Z` 볼륨 레이블을 사용하여
workspace/config 마운트가 컨테이너 내에서 쓰기 가능하도록 합니다.

`--skip-build`를 추가하면, 설치 프로그램이 로컬 이미지 빌드를 건너뜁니다. 먼저 로컬
Docker 태그 (`ZEROCLAW_DOCKER_IMAGE`, 기본값: `zeroclaw-bootstrap:local`)를 시도하고,
없으면 `ghcr.io/zeroclaw-labs/zeroclaw:latest`를 pull한 후 로컬에 태그합니다.

### Docker/Podman 컨테이너 중지 및 재시작

`./install.sh --docker`가 완료되면 컨테이너가 종료됩니다. config와 workspace는
데이터 디렉터리에 유지됩니다 (기본값: `./.zeroclaw-docker`, `curl | bash`로 부트스트랩할 때는
`~/.zeroclaw-docker`). `ZEROCLAW_DOCKER_DATA_DIR`로 이 경로를 재정의할 수 있습니다.

재시작하려면 **`install.sh`를 다시 실행하지 마세요** -- 이미지를 다시 빌드하고 온보딩을 재실행합니다.
대신, 기존 이미지에서 새 컨테이너를 시작하고 유지된 데이터 디렉터리를 마운트하세요.

#### 저장소 docker-compose.yml 사용

Docker/Podman에서 ZeroClaw를 장기 실행하는 가장 간단한 방법은 저장소 루트에 제공된
`docker-compose.yml`을 사용하는 것입니다. 이 파일은 named volume (`zeroclaw-data`)을 사용하고
`restart: unless-stopped`를 설정하여 컨테이너가 재부팅 후에도 유지됩니다.

```bash
# 시작 (백그라운드)
docker compose up -d

# 중지
docker compose down

# 중지 후 재시작
docker compose up -d
```

Podman을 사용하는 경우 `docker`를 `podman`으로 교체하세요.

#### 수동 컨테이너 실행 (install.sh 데이터 디렉터리 사용)

`./install.sh --docker`로 설치했고 compose 없이 `.zeroclaw-docker`
데이터 디렉터리를 재사용하려면:

```bash
# Docker
docker run -d --name zeroclaw \
  --restart unless-stopped \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway

# Podman (--userns keep-id 및 :Z 볼륨 레이블 추가)
podman run -d --name zeroclaw \
  --restart unless-stopped \
  --userns keep-id \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw:Z" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace:Z" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

#### 공통 라이프사이클 명령어

```bash
# 컨테이너 중지 (데이터 보존)
docker stop zeroclaw

# 중지된 컨테이너 시작 (config와 workspace 유지)
docker start zeroclaw

# 로그 보기
docker logs -f zeroclaw

# 컨테이너 제거 (볼륨/.zeroclaw-docker의 데이터는 보존)
docker rm zeroclaw

# 상태 확인
docker exec zeroclaw zeroclaw status
```

#### 환경 변수

수동 실행 시, provider 구성을 환경 변수로 전달하거나
유지된 `config.toml`에 이미 저장되어 있는지 확인하세요:

```bash
docker run -d --name zeroclaw \
  -e API_KEY="sk-..." \
  -e PROVIDER="openrouter" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

초기 설치 중에 `onboard`를 이미 실행했다면, API 키와 provider가
`.zeroclaw-docker/.zeroclaw/config.toml`에 저장되어 있으므로 다시 전달할 필요가 없습니다.

### 빠른 온보딩 (비대화형)

```bash
./install.sh --api-key "sk-..." --provider openrouter
```

또는 환경 변수를 사용하여:

```bash
ZEROCLAW_API_KEY="sk-..." ZEROCLAW_PROVIDER="openrouter" ./install.sh
```

## 유용한 플래그

- `--install-system-deps`
- `--install-rust`
- `--skip-build` (`--docker` 모드에서: 로컬 이미지가 있으면 사용, 없으면 `ghcr.io/zeroclaw-labs/zeroclaw:latest` pull)
- `--skip-install`
- `--provider <id>`

모든 옵션 보기:

```bash
./install.sh --help
```

## 관련 문서

- [README.md](../README.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
- [providers-reference.md](../reference/api/providers-reference.md)
- [channels-reference.md](../reference/api/channels-reference.md)
