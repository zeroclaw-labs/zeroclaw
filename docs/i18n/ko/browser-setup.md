# 브라우저 자동화 설정 가이드

이 가이드에서는 headless 자동화와 VNC를 통한 GUI 접근을 포함하여 ZeroClaw에서 브라우저 자동화 기능을 설정하는 방법을 다룹니다.

## 개요

ZeroClaw는 다양한 브라우저 접근 방법을 지원합니다:

| 방법 | 사용 사례 | 요구 사항 |
|--------|----------|--------------|
| **agent-browser CLI** | Headless 자동화, AI 에이전트 | npm, Chrome |
| **VNC + noVNC** | GUI 접근, 디버깅 | Xvfb, x11vnc, noVNC |
| **Chrome Remote Desktop** | Google을 통한 원격 GUI | XFCE, Google 계정 |

## 빠른 시작: Headless 자동화

### 1. agent-browser 설치

```bash
# CLI 설치
npm install -g agent-browser

# Chrome for Testing 다운로드
agent-browser install --with-deps  # Linux (시스템 종속성 포함)
agent-browser install              # macOS/Windows
```

### 2. ZeroClaw 설정 확인

브라우저 도구는 기본적으로 활성화되어 있습니다. 확인하거나 커스터마이징하려면
`~/.zeroclaw/config.toml`을 편집하세요:

```toml
[browser]
enabled = true              # 기본값: true
allowed_domains = ["*"]     # 기본값: ["*"] (모든 공개 호스트)
backend = "agent_browser"   # 기본값: "agent_browser"
native_headless = true      # 기본값: true
```

도메인을 제한하거나 브라우저 도구를 비활성화하려면:

```toml
[browser]
enabled = false                              # 완전히 비활성화
# 또는 특정 도메인으로 제한:
allowed_domains = ["example.com", "docs.example.com"]
```

### 3. 테스트

```bash
echo "Open https://example.com and tell me what it says" | zeroclaw agent
```

## VNC 설정 (GUI 접근)

디버깅이 필요하거나 시각적인 브라우저 접근이 필요한 경우:

### 종속성 설치

```bash
# Ubuntu/Debian
apt-get install -y xvfb x11vnc fluxbox novnc websockify

# 선택 사항: Chrome Remote Desktop을 위한 데스크톱 환경
apt-get install -y xfce4 xfce4-goodies
```

### VNC 서버 시작

```bash
#!/bin/bash
# VNC 접근이 가능한 가상 디스플레이 시작

DISPLAY_NUM=99
VNC_PORT=5900
NOVNC_PORT=6080
RESOLUTION=1920x1080x24

# Xvfb 시작
Xvfb :$DISPLAY_NUM -screen 0 $RESOLUTION -ac &
sleep 1

# 윈도우 매니저 시작
fluxbox -display :$DISPLAY_NUM &
sleep 1

# x11vnc 시작
x11vnc -display :$DISPLAY_NUM -rfbport $VNC_PORT -forever -shared -nopw -bg
sleep 1

# noVNC 시작 (웹 기반 VNC)
websockify --web=/usr/share/novnc $NOVNC_PORT localhost:$VNC_PORT &

echo "VNC available at:"
echo "  VNC Client: localhost:$VNC_PORT"
echo "  Web Browser: http://localhost:$NOVNC_PORT/vnc.html"
```

### VNC 접근

- **VNC 클라이언트**: `localhost:5900`에 연결
- **웹 브라우저**: `http://localhost:6080/vnc.html` 열기

### VNC 디스플레이에서 브라우저 시작

```bash
DISPLAY=:99 google-chrome --no-sandbox https://example.com &
```

## Chrome Remote Desktop

### 설치

```bash
# 다운로드 및 설치
wget https://dl.google.com/linux/direct/chrome-remote-desktop_current_amd64.deb
apt-get install -y ./chrome-remote-desktop_current_amd64.deb

# 세션 구성
echo "xfce4-session" > ~/.chrome-remote-desktop-session
chmod +x ~/.chrome-remote-desktop-session
```

### 설정

1. <https://remotedesktop.google.com/headless>를 방문합니다
2. "Debian Linux" 설정 명령을 복사합니다
3. 서버에서 해당 명령을 실행합니다
4. 서비스를 시작합니다: `systemctl --user start chrome-remote-desktop`

### 원격 접근

아무 기기에서 <https://remotedesktop.google.com/access>로 이동합니다.

## 테스트

### CLI 테스트

```bash
# 기본 열기 및 닫기
agent-browser open https://example.com
agent-browser get title
agent-browser close

# 참조 포함 스냅샷
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser close

# 스크린샷
agent-browser open https://example.com
agent-browser screenshot /tmp/test.png
agent-browser close
```

### ZeroClaw 통합 테스트

```bash
# 콘텐츠 추출
echo "Open https://example.com and summarize it" | zeroclaw agent

# 내비게이션
echo "Go to https://github.com/trending and list the top 3 repos" | zeroclaw agent

# 양식 상호작용
echo "Go to Wikipedia, search for 'Rust programming language', and summarize" | zeroclaw agent
```

## 문제 해결

### "Element not found"

페이지가 완전히 로드되지 않았을 수 있습니다. 대기를 추가하세요:

```bash
agent-browser open https://slow-site.com
agent-browser wait --load networkidle
agent-browser snapshot -i
```

### 쿠키 다이얼로그가 접근을 차단하는 경우

먼저 쿠키 동의를 처리하세요:

```bash
agent-browser open https://site-with-cookies.com
agent-browser snapshot -i
agent-browser click @accept_cookies  # 수락 버튼 클릭
agent-browser snapshot -i  # 이제 실제 콘텐츠를 가져옵니다
```

### Docker sandbox 네트워크 제한

Docker sandbox 내에서 `web_fetch`가 실패하면 agent-browser를 대신 사용하세요:

```bash
# web_fetch 대신 다음을 사용:
agent-browser open https://example.com
agent-browser get text body
```

## 보안 참고 사항

- `agent-browser`는 샌드박싱이 적용된 headless 모드로 Chrome을 실행합니다
- 민감한 사이트의 경우 `--session-name`을 사용하여 인증 상태를 유지하세요
- `--allowed-domains` 설정은 특정 도메인으로 내비게이션을 제한합니다
- VNC 포트(5900, 6080)는 방화벽 또는 Tailscale 뒤에 있어야 합니다

## 관련 문서

- [agent-browser 문서](https://github.com/vercel-labs/agent-browser)
- [ZeroClaw 설정 레퍼런스](./config-reference.md)
- [Skills 문서](../skills/)
