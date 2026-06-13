<!-- Canonical deployment-shape diagram. Edit here; reuse via {{#include}}. -->
```text
zeroclaw service                          — systemd / launchctl / Windows Service
└── zeroclaw daemon                       — the single long-running process
    ├── gateway listener  :42617          — REST / WebSocket / webhook intake
    ├── channel pollers                   — Telegram, IMAP, Nostr relays (outbound poll)
    ├── channel listeners                 — Discord / Slack / Matrix / WebSocket (inbound stream)
    ├── cron scheduler                    — scheduled SOPs and jobs
    └── agent loop  (one per session)     — provider call + tool execution
                                            ▲ driven by any listener, poller,
                                              gateway request, or cron fire

on disk (everything but the binary can move)
├── ~/.zeroclaw/config.toml               — configuration
├── ~/.zeroclaw/.secret_key               — master key for the encrypted secrets store
└── ~/.zeroclaw/data/                     — runtime state
    ├── memory/                           — agent memory backend
    ├── sessions/                         — per-session conversation stores
    └── state/                            — scheduler, cost, health, misc runtime state

logs                                      — journald / launchctl / Windows Event Log (platform-native)
```
