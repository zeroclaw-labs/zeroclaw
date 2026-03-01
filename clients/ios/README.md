# ZeroClaw iOS Client

Native iOS client for ZeroClaw — run your autonomous AI assistant on iPhone.

## Features

- **Native Performance** — SwiftUI, not a webview
- **iOS 26 Liquid Glass** — Native glass effect throughout the UI
- **Gateway Integration** — WebSocket real-time chat with ZeroClaw gateway
- **Streaming Responses** — Live token-by-token output from the agent
- **Security First** — iOS Keychain for API key and bearer token storage
- **Pairing** — One-time code pairing with gateway
- **Management Dashboard** — Memory, cron jobs, tools, cost, devices, integrations
- **Markdown Rendering** — Rich message formatting with code blocks
- **Background Tasks** — Periodic health checks via BGTaskScheduler
- **Notifications** — Local notifications for messages and status
- **WidgetKit** — Home screen widget with agent status
- **Share Extension** — Share text/URLs/images to ZeroClaw
- **Siri Shortcuts** — "Ask ZeroClaw", "Check Status", "Toggle Agent"
- **Dark Mode** — System, light, and dark theme support

## Requirements

- iOS 26.0+ (iPhone 15 Pro and newer)
- Xcode 26+
- A running ZeroClaw gateway to connect to

## Building

### Build App

Open `clients/ios/ZeroClaw.xcodeproj` in Xcode and run (Cmd+R), or:

```bash
open -a Xcode clients/ios/ZeroClaw.xcodeproj
```

### Optional: Build XCFramework (Rust bridge)

Only needed if you want to embed the Rust runtime directly (not required for gateway client mode):

```bash
# Install Rust iOS targets
rustup target add aarch64-apple-ios aarch64-apple-ios-sim

cd clients/ios-bridge
./build-ios.sh
```

## Architecture

The iOS app operates as a thin network client connecting to the ZeroClaw gateway. All agent logic, tool execution, memory, and model routing happen server-side.

```
┌─────────────────────────────────────┐
│  UI (SwiftUI + Liquid Glass)        │
│  ├─ Chat (markdown, tool calls)     │
│  ├─ Management dashboard            │
│  └─ Settings + pairing onboarding   │
├─────────────────────────────────────┤
│  Services (Swift)                   │
│  ├─ GatewayClient (WebSocket/HTTP)  │
│  ├─ AgentService (state + streaming)│
│  ├─ SettingsManager (Keychain)      │
│  ├─ ChatStore (persistence)         │
│  ├─ NotificationManager             │
│  └─ BackgroundTaskManager           │
├─────────────────────────────────────┤
│  Gateway (Network)                  │
│  POST /pair, GET /ws/chat,          │
│  POST /api/chat, GET /api/events,   │
│  GET /api/status, GET /api/tools    │
└─────────────────────────────────────┘
```

### Project Structure

```
clients/ios/
├── ZeroClaw.xcodeproj/
├── ZeroClaw/
│   ├── ZeroClawApp.swift              # App entry point + lifecycle
│   ├── ContentView.swift              # Main chat view + navigation
│   ├── Info.plist                     # Background modes + ATS
│   ├── Views/
│   │   ├── ChatMessageView.swift      # Chat bubbles + markdown
│   │   ├── ChatInputView.swift        # Floating input bar
│   │   ├── MarkdownView.swift         # Markdown renderer
│   │   ├── ToolCallBubble.swift       # Tool call/result display
│   │   ├── SettingsView.swift         # Settings + management nav
│   │   ├── PairingOnboardingView.swift # Gateway pairing flow
│   │   ├── StatusDetailView.swift     # Diagnostics modal
│   │   ├── StatusIndicatorView.swift  # Connection badge
│   │   ├── EmptyStateView.swift       # Welcome state
│   │   ├── MemoryView.swift           # Knowledge base browser
│   │   ├── CronJobsView.swift         # Scheduled tasks
│   │   ├── ToolsBrowserView.swift     # Tool catalog
│   │   ├── CostView.swift             # Usage & cost tracking
│   │   ├── PairedDevicesView.swift     # Device management
│   │   └── IntegrationsView.swift     # Integration status
│   ├── Models/
│   │   ├── ChatMessage.swift          # Message extensions
│   │   ├── AgentStatus.swift          # Status display
│   │   └── GatewayModels.swift        # API response types
│   ├── Services/
│   │   ├── GatewayClient.swift        # WebSocket + HTTP client
│   │   ├── AgentService.swift         # State + streaming orchestration
│   │   ├── SettingsManager.swift      # Keychain + UserDefaults
│   │   ├── ChatStore.swift            # File-based chat persistence
│   │   ├── KeychainHelper.swift       # Keychain CRUD wrapper
│   │   ├── NotificationManager.swift  # Local notifications
│   │   └── BackgroundTaskManager.swift # BGTaskScheduler
│   ├── Intents/
│   │   ├── ZeroClawIntents.swift      # App Intents (Siri)
│   │   └── ZeroClawShortcuts.swift    # Shortcut phrases
│   └── Theme/
│       └── Theme.swift
├── ZeroClawWidget/                    # WidgetKit extension
│   └── ZeroClawWidget.swift
├── ZeroClawShare/                     # Share extension
│   ├── ShareViewController.swift
│   └── Info.plist
└── ZeroClawTests/
```

## Status

**Phase 1: Foundation** (Complete)
- [x] Xcode project setup (SwiftUI)
- [x] Core models and services

**Phase 2: UI** (Complete)
- [x] Chat interface with message bubbles
- [x] Floating input bar with glass effect
- [x] Settings screen (provider, model, API key)
- [x] Status indicator
- [x] Empty state with onboarding
- [x] iOS 26 Liquid Glass theme
- [x] Dark/light/system theme picker

**Phase 3: Integration** (Complete)
- [x] Gateway client (WebSocket + HTTP, pure Swift)
- [x] Gateway pairing (one-time code, bearer token in Keychain)
- [x] Agent service rewrite (streaming, auto-reconnect, event-driven)
- [x] Chat persistence (file-based JSON per session)
- [x] Markdown rendering with code blocks
- [x] Tool call/result display
- [x] Management dashboard (memory, cron, tools, cost, devices, integrations)
- [x] Pairing onboarding flow
- [x] Background task scheduling (BGTaskScheduler)
- [x] Local notifications (message + status)

**Phase 4: Extensions** (Complete)
- [x] WidgetKit widget (agent status + last message)
- [x] Share Extension (text, URLs, images)
- [x] App Intents / Siri shortcuts (Ask, Status, Toggle)

## License

Same as ZeroClaw (MIT/Apache-2.0)
