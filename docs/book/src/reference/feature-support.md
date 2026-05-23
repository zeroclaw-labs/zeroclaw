# Feature support matrix

This page is a compact map of the ZeroClaw surface and how it relates to nearby Claw-family projects. It shows what ZeroClaw supports today, what is experimental or partial, and where to read the canonical docs. It is a starting point for users choosing a setup, not a replacement for the detailed setup guides.

When a row says "planned" or "needs verification", it is not claiming current support. Follow the linked issue, RFC, or docs page before building on that area.

## Status legend

| Status | Meaning |
|---|---|
| Supported | Documented, implemented, and expected to work in the current release when configured correctly. |
| Experimental | Implemented, but still behind feature flags, active architecture work, or a less-stable runtime surface. |
| Partial | A useful subset works, but important modes, integrations, or user-facing docs are still missing. |
| Planned | Accepted or tracked work exists, but the capability should not be treated as implemented yet. |
| Needs verification | The project needs a current source/docs pass before making a support claim. |
| Not planned | The project does not currently intend to support this shape. |

## Project comparison

The original feature-matrix request was also a comparison request: users need to know when to choose ZeroClaw instead of OpenClaw, PicoClaw, NanoBot, or a hosted/cloud deployment shape. This table keeps that comparison factual. It links to external project docs where the source of truth lives and avoids claiming parity that this repository has not audited.

| Option | Best fit | Current relationship to ZeroClaw | Evidence |
|---|---|---|---|
| ZeroClaw | A user-owned Rust agent runtime for local machines, services, gateways, VPS/cloud VMs, SBCs, hardware/peripheral experiments, and plugin-oriented future work. | This page documents ZeroClaw's current support surface. ZeroClaw owns the support claims below. | [Project philosophy](../philosophy.md), [Quick start](../getting-started/quick-start.md), [FND-001](../foundations/fnd-001-intentional-architecture.md) |
| OpenClaw | Legacy/source project context for users coming from the earlier TypeScript runtime. | ZeroClaw was bootstrapped from OpenClaw code, but this page does not yet claim full feature parity. A dedicated parity audit is still needed before closing the comparison request. | [FND-001](../foundations/fnd-001-intentional-architecture.md), [Issue #6810](https://github.com/zeroclaw-labs/zeroclaw/issues/6810) |
| PicoClaw | External Go runtime focused on very low resource use, broad architecture support, and small hardware deployments. | PicoClaw is a related external project, not a ZeroClaw compatibility target. Use PicoClaw docs for PicoClaw feature claims; use this page for ZeroClaw claims. | [PicoClaw repository](https://github.com/sipeed/picoclaw), [PicoClaw hardware list](https://github.com/sipeed/picoclaw/blob/main/docs/hardware-compatibility.md) |
| NanoBot | External Python lightweight assistant inspired by OpenClaw, useful as another comparison point for minimal assistant runtimes. | NanoBot is related comparison context only. Current ZeroClaw docs do not define a NanoBot compatibility target. | [NanoBot repository](https://github.com/HKUDS/nanobot) |
| Hosted or managed cloud alternatives | Users who want a managed service rather than a self-owned runtime. | ZeroClaw supports running on a VPS/cloud VM and integrating hosted model providers, but the docs do not describe ZeroClaw itself as a managed cloud service. | [Project philosophy](../philosophy.md), [Docker & containers](../setup/container.md), [Network deployment](../ops/network-deployment.md) |

Comparison status:

- **Covered here:** ZeroClaw's current runtime, deployment, provider, channel, tool, security, hardware, and plugin support.
- **Partially covered here:** the high-level relationship between ZeroClaw, OpenClaw, PicoClaw, NanoBot, and cloud-hosted deployment shapes.
- **Still deferred:** a row-by-row OpenClaw-to-ZeroClaw parity audit. That should compare concrete user-facing capabilities, not only roadmap documents.

## Runtime and deployment

| Area | Status | Notes | Evidence |
|---|---|---|---|
| Local CLI agent | Supported | The default way to install, onboard, configure, and run ZeroClaw locally. | [Quick start](../getting-started/quick-start.md) |
| Linux install | Supported | `install.sh`, prebuilt/source install, systemd service, and SBC notes are documented. | [Linux setup](../setup/linux.md), [Service management](../setup/service.md) |
| macOS install | Supported | `install.sh`, Homebrew, LaunchAgent service, and Apple Silicon/Intel release notes are documented. | [macOS setup](../setup/macos.md), [Service management](../setup/service.md) |
| Windows install | Supported | `setup.bat`, Scoop, source install, scheduled task, and Windows Service paths are documented. | [Windows setup](../setup/windows.md), [Service management](../setup/service.md) |
| Docker / OCI container | Supported | Official GHCR images, Compose, Kubernetes fragments, webhook ingress, and container gotchas are documented. | [Docker & containers](../setup/container.md), [Network deployment](../ops/network-deployment.md) |
| Daemon / service mode | Supported | User/system service scopes, restart behavior, shutdown grace, resource limits, and multi-workspace operation are documented. | [Service & daemon](../ops/service.md) |
| Gateway REST / WebSocket API | Supported | The gateway exposes REST, WebSocket, pairing/bearer auth, runtime-generated OpenAPI docs, and config mutation endpoints. | [Gateway HTTP API](../gateway/api.md) |
| Web dashboard | Supported | The gateway/dashboard API is supported; serving the UI requires a bundled or configured `web/dist`. | [Building the web dashboard](../developing/web.md), [Docker & containers](../setup/container.md) |
| ACP editor / IDE sessions | Supported | ACP works over stdio and over the daemon gateway WebSocket, with persisted sessions and permission prompts. | [ACP channel](../channels/acp.md) |
| Tauri desktop app | Partial | Desktop build/release machinery exists, but the user-facing install and support story is not yet covered like Linux/macOS/Windows setup. | [Release runbook](../maintainers/release-runbook.md), [Issue #6810](https://github.com/zeroclaw-labs/zeroclaw/issues/6810) |
| Raspberry Pi / SBC | Experimental | Raspberry Pi is documented for gateway and hardware use, but hardware builds require feature flags and device permissions. | [Raspberry Pi](../hardware/raspberry-pi-setup.md), [Hardware overview](../hardware/index.md) |
| Horizontal scaling | Not planned | One ZeroClaw instance owns one workspace; run separate instances for separate agents/workspaces instead of multiple writers against one workspace. | [Docker & containers](../setup/container.md), [Service & daemon](../ops/service.md) |

## Providers and model behavior

| Area | Status | Notes | Evidence |
|---|---|---|---|
| Native provider families | Supported | Typed provider slots are documented in the provider catalog, which owns the current provider list. | [Provider catalog](../providers/catalog.md) |
| OpenAI-compatible providers | Supported | Many compatible vendors have canonical slots; unknown compatible endpoints can use the `custom` slot with `uri`. | [Provider catalog](../providers/catalog.md), [Custom providers](../providers/custom.md) |
| Local providers | Supported | Local and local-server provider slots are documented in the provider catalog. | [Provider catalog](../providers/catalog.md) |
| Per-agent provider selection | Supported | Providers are referenced from agents by alias; there is no global default provider. | [Providers overview](../providers/overview.md) |
| Streaming text | Supported | Providers that speak streaming APIs emit token deltas, and channels that support drafts can surface partial updates. | [Streaming](../providers/streaming.md) |
| Streaming tool calls | Supported | The runtime supports mid-stream tool calls, but compatible providers differ in whether they stream tool-call arguments or emit completed calls. | [Streaming](../providers/streaming.md) |
| Reasoning / thinking output | Partial | Reasoning deltas exist for supported models and are hidden from users by default; provider-specific behavior still varies. | [Streaming](../providers/streaming.md), [Provider catalog](../providers/catalog.md) |
| Vision | Partial | Native vision support is documented for some providers, but provider and model coverage is not uniform across the catalog. | [Provider catalog](../providers/catalog.md) |
| Provider-side grounded search | Partial | Gemini can emit pre-executed grounded-search events; this is not a universal provider capability. | [Streaming](../providers/streaming.md) |
| Subscription / OAuth-style provider auth | Partial | Some provider entries support OAuth or subscription tokens, but coverage is provider-specific. | [Providers overview](../providers/overview.md), [Provider catalog](../providers/catalog.md) |

## Channels

Channel rows assume a build that includes the matching feature flag; the channels overview owns the current feature list.

| Area | Status | Notes | Evidence |
|---|---|---|---|
| CLI channel | Supported | Local stdin/stdout operation is always available. | [Channels overview](../channels/overview.md) |
| Gateway REST / WebSocket channel | Supported | Gateway clients can drive the agent through HTTP/WebSocket and share the same config mutation core as the CLI. | [Channels overview](../channels/overview.md), [Gateway HTTP API](../gateway/api.md) |
| Webhooks | Supported | Webhooks live under the gateway and can be exposed through reverse proxy, tunnel, or explicit public bind. | [Webhooks](../channels/webhook.md), [Network deployment](../ops/network-deployment.md) |
| Matrix | Supported | Matrix has a dedicated guide. | [Matrix](../channels/matrix.md) |
| Mattermost | Supported | Mattermost has a dedicated guide. | [Mattermost](../channels/mattermost.md) |
| LINE | Supported | LINE has a dedicated guide. | [LINE](../channels/line.md) |
| Nextcloud Talk | Supported | Nextcloud Talk has a dedicated guide and uses gateway ingress. | [Nextcloud Talk](../channels/nextcloud-talk.md) |
| Other chat platforms | Partial | The other-chat guide owns the current platform list. Some use polling, some require gateway ingress, and feature depth varies by channel. | [Other chat platforms](../channels/chat-others.md) |
| Social / broadcast channels | Supported | Public-feed integrations are documented as social/broadcast channels. | [Social channels](../channels/social.md) |
| Email | Partial | IMAP/SMTP and Gmail Push are documented; the channel guide still needs fuller coverage of advanced outbound behavior. | [Email](../channels/email.md) |
| Voice and telephony | Experimental | Telnyx/Twilio/Plivo voice paths and local wake-word/TTS channels are documented, but they require provider/account setup and careful deployment. | [Voice & telephony](../channels/voice.md) |
| Channel streaming drafts | Partial | The streaming guide owns the current draft-update support matrix. | [Streaming](../providers/streaming.md) |
| Pairing for public channels | Supported | Pairing is required for most channels to bind incoming identities to policy. | [Channels overview](../channels/overview.md) |

## Tools, skills, and plugins

| Area | Status | Notes | Evidence |
|---|---|---|---|
| Shell / file / HTTP tools | Supported | Built-in tools include shell, file read/write/list, HTTP, time, memory, user asks, and human escalation. | [Tools overview](../tools/overview.md), [Security overview](../security/overview.md) |
| Web search and HTTP/browser fetch | Supported | Web search, HTTP, browser automation, and PDF extraction are documented tool families. | [Tools overview](../tools/overview.md), [Browser automation](../tools/browser.md) |
| Browser automation | Supported | Browser automation is documented as a built-in tool with setup notes. | [Browser automation](../tools/browser.md) |
| Memory tools | Supported | Memory search and memory pin are built-in tools; memory storage and retrieval are part of the runtime architecture. | [Tools overview](../tools/overview.md), [Architecture overview](../architecture/overview.md) |
| SOP tools | Supported | SOP documents and `sop_*` tools are documented when SOP is configured. | [Tools overview](../tools/overview.md), [SOP overview](../sop/index.md) |
| Cron tools | Supported | Cron tools manage scheduled jobs. | [Tools overview](../tools/overview.md) |
| MCP tools | Supported | ZeroClaw can connect MCP servers and load their tools at startup. | [MCP](../tools/mcp.md), [Tools overview](../tools/overview.md) |
| Agent Skills | Supported | Local `SKILL.md` / `SKILL.toml`, list/audit/install/remove/test commands, compact loading, and opt-in open-skills loading are documented. | [Skills](../tools/skills.md) |
| Prompt-triggered skill install suggestions | Partial | Server-side suggestions can point to cached registry metadata, but plugin/package discovery and composer-time suggestions are follow-up scope. | [Skills](../tools/skills.md) |
| WASM tool plugins | Experimental | The plugin protocol documents WASM tool plugins and host functions, but several permission and non-tool capability surfaces are not implemented yet. | [Plugin protocol](../developing/plugin-protocol.md) |
| Skill-only plugins | Experimental | Skill-only plugin bundles can ship agentskills.io-format skills without a WASM payload. | [Plugin protocol](../developing/plugin-protocol.md) |

## Hardware and edge use

| Area | Status | Notes | Evidence |
|---|---|---|---|
| Hardware feature flag | Experimental | Hardware support must be enabled with `--features hardware` or narrower board features. | [Hardware overview](../hardware/index.md) |
| GPIO / I2C / SPI tools | Experimental | Runtime hardware tools exist when the feature is enabled and device paths are configured. | [Hardware overview](../hardware/index.md) |
| STM32 Nucleo | Experimental | Dedicated setup guide exists. | [STM32 Nucleo](../hardware/nucleo-setup.md) |
| Arduino Uno Q | Experimental | Dedicated setup guide exists. | [Arduino Uno Q](../hardware/arduino-uno-q-setup.md) |
| Raspberry Pi hardware use | Experimental | Raspberry Pi GPIO/I2C/SPI setup is documented, including service group membership. | [Raspberry Pi](../hardware/raspberry-pi-setup.md), [Hardware overview](../hardware/index.md) |
| Aardvark I2C/SPI adapter | Experimental | Dedicated setup guide exists. | [Aardvark](../hardware/aardvark.md) |
| Android / Termux hardware path | Experimental | Android serial-over-USB / Bluetooth notes exist, with the Play Store version called out as unsupported. | [Android](../hardware/android-setup.md) |

## Security and policy

| Area | Status | Notes | Evidence |
|---|---|---|---|
| Channel pairing and allow lists | Supported | Pairing and channel authorization run before public channel events reach the agent runtime. | [Security overview](../security/overview.md), [Channels overview](../channels/overview.md) |
| Autonomy levels | Supported | Autonomy levels gate which tool risks can run without operator approval. | [Autonomy levels](../security/autonomy.md) |
| Workspace and path boundaries | Supported | File and shell tools are constrained by workspace and sandbox policy. | [Security overview](../security/overview.md), [Sandboxing](../security/sandboxing.md) |
| OS sandboxing | Partial | Linux, macOS, Windows, and Docker backends differ by platform and maturity. | [Sandboxing](../security/sandboxing.md) |
| Tool receipts | Supported | Tool invocations produce receipt records for auditability. | [Tool receipts](../security/tool-receipts.md) |
| OTP and emergency stop gates | Supported | Optional gates can require one-time codes or interrupt unsafe operation. | [Security overview](../security/overview.md) |

## Related runtimes and roadmap positioning

| Area | Status | Notes | Evidence |
|---|---|---|---|
| OpenClaw parity / migration notes | Partial | This page now gives comparison context and links the current migration position, but a full feature-by-feature OpenClaw parity audit remains future work. | [Issue #6810](https://github.com/zeroclaw-labs/zeroclaw/issues/6810), [FND-001](../foundations/fnd-001-intentional-architecture.md) |
| PicoClaw / NanoBot lightweight deployments | Partial | The comparison table links the external projects and explains their relationship to ZeroClaw, but ZeroClaw-owned docs should not mirror their full feature lists. | [PicoClaw repository](https://github.com/sipeed/picoclaw), [NanoBot repository](https://github.com/HKUDS/nanobot) |
| Future plugin/channel/memory/observer capability plugins | Planned | The plugin manifest includes these capability categories, but several are explicitly marked not yet implemented. | [Plugin protocol](../developing/plugin-protocol.md) |

## Keeping this page current

When changing support status, update the linked evidence at the same time. If a row cannot point to docs, an issue, an RFC/FND, or source-backed reference, mark it "needs verification" instead of guessing. The linked evidence remains authoritative for exact provider, channel, tool, and setup details; this page owns the high-level support classification.
