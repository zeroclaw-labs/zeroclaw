# iOS Setup

ZeroClaw provides a native iOS client that connects to a running ZeroClaw gateway.

## Requirements

- iOS 26.0+ (iPhone 15 Pro and newer)
- Xcode 26+
- A running ZeroClaw gateway (local or remote)

## Quick Start

1. Open the project in Xcode:

```bash
open -a Xcode clients/ios/ZeroClaw.xcodeproj
```

2. Build and run on device or simulator (Cmd+R).

3. On first launch, enter your gateway's host and port, then pair using a one-time code from the gateway.

## Gateway Pairing

The iOS client authenticates with the gateway via a one-time pairing code:

1. Start your ZeroClaw gateway (`zeroclaw serve`).
2. Open the iOS app — the pairing screen appears on first launch.
3. Enter the gateway host (default `127.0.0.1`) and port (default `42617`).
4. Enter the pairing code shown in the gateway logs.
5. The app exchanges the code for a bearer token stored securely in iOS Keychain.

## Features

| Feature | Description |
|---------|-------------|
| Real-time chat | WebSocket streaming with token-by-token output |
| Management | Memory, cron jobs, tools, cost, devices, integrations |
| Markdown | Rich message rendering with code blocks |
| Notifications | Local notifications for messages when backgrounded |
| Background health | Periodic gateway health checks via BGTaskScheduler |
| Widget | Home screen widget showing agent status |
| Share Extension | Share text/URLs/images from other apps |
| Siri Shortcuts | "Ask ZeroClaw", "Check Status" voice commands |

## Architecture

The app is a thin network client — all agent logic runs on the gateway. Communication uses:

- **WebSocket** (`/ws/chat`) for real-time chat streaming
- **HTTP** (`/api/*`) for management operations (memory, cron, tools, etc.)
- **SSE** (`/api/events`) for background status monitoring

See [clients/ios/README.md](../clients/ios/README.md) for full project structure and build details.
