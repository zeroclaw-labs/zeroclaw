# Feature support matrix

This page compares ZeroClaw support against nearby Claw-family runtimes and deployment shapes. It is meant to answer "which claw supports this?" quickly, then point readers to the canonical docs for details.

The matrix is conservative. A row marked "planned" or "needs verification" is not a current support claim; follow the linked docs, issue, or foundation note before relying on it.

## Status legend

| Status | Meaning |
|---|---|
| Supported | Documented, implemented, and expected to work in the current release when configured correctly. |
| Experimental | Implemented, but still behind feature flags, active architecture work, or a less-stable runtime surface. |
| Partial | A useful subset works, but important modes, integrations, or user-facing docs are still missing. |
| Planned | Accepted or tracked work exists, but the capability should not be treated as implemented yet. |
| Needs verification | The project needs a current source/docs pass before making a support claim. |
| Not planned | The project does not currently intend to support this shape. |

## Runtime and deployment matrix

| Capability | ZeroClaw | OpenClaw | PicoClaw | NanoClaw-style lightweight runtime | Hosted/cloud shape | Evidence |
|---|---|---|---|---|---|---|
| Primary fit | Supported: user-owned Rust agent runtime for local machines, services, gateways, VPS/cloud VMs, SBCs, hardware experiments, and plugin-oriented future work. | Needs verification: legacy/source project context for users coming from the earlier TypeScript runtime; this page does not claim full parity. | Partial: related external Go runtime focused on small deployments; use PicoClaw docs for PicoClaw claims. | Needs verification: comparison category for minimal assistant runtimes, not a ZeroClaw compatibility target. | Partial: ZeroClaw can run on cloud infrastructure, but the project does not describe itself as a managed service. | [Project philosophy](../philosophy.md), [Quick start](../getting-started/quick-start.md), [FND-001](../foundations/fnd-001-intentional-architecture.md), [PicoClaw repository](https://github.com/sipeed/picoclaw), [Issue #6810](https://github.com/zeroclaw-labs/zeroclaw/issues/6810) |
| Local CLI agent | Supported: default local install, onboarding, configuration, and run path. | Needs verification: parity audit still needed. | Needs verification: external runtime; check PicoClaw docs. | Needs verification. | Not planned: hosted-only operation is not ZeroClaw's local CLI shape. | [Quick start](../getting-started/quick-start.md) |
| Desktop OS installs | Supported: Linux, macOS, Windows, and FreeBSD setup docs exist. | Needs verification. | Needs verification. | Needs verification. | Partial: can be installed on cloud VMs where OS support matches. | [Linux](../setup/linux.md), [macOS](../setup/macos.md), [Windows](../setup/windows.md), [FreeBSD](../setup/freebsd.md) |
| Daemon or service mode | Supported: user/system service scopes, restart behavior, shutdown grace, and resource limits are documented. | Needs verification. | Needs verification. | Needs verification. | Supported: service mode is the normal VPS/cloud VM shape. | [Service management](../setup/service.md), [Service & daemon](../ops/service.md) |
| Gateway and web UI | Supported: REST, WebSocket, pairing/bearer auth, runtime-generated OpenAPI docs, and dashboard serving are documented. | Needs verification. | Needs verification. | Needs verification. | Supported when the operator exposes the gateway through a reverse proxy, tunnel, or public bind. | [Gateway HTTP API](../gateway/api.md), [Web dashboard](../gateway/web-dashboard.md), [Network deployment](../ops/network-deployment.md) |
| Container / OCI deployment | Supported: GHCR images, Compose, Kubernetes fragments, and ingress notes are documented. | Needs verification. | Needs verification. | Needs verification. | Supported: container deployment is the clearest cloud/VPS packaging path. | [Docker & containers](../setup/container.md), [Network deployment](../ops/network-deployment.md) |
| Desktop app / Tauri | Partial: desktop build and release machinery exists, but the user-facing install/support story is not yet on the same footing as OS setup docs. | Needs verification. | Not planned by ZeroClaw docs. | Needs verification. | Not planned: this is not a managed cloud shape. | [Release runbook](../maintainers/release-runbook.md), [Issue #6810](https://github.com/zeroclaw-labs/zeroclaw/issues/6810) |
| SBC / edge deployment | Experimental: Raspberry Pi and hardware docs exist, but hardware builds require feature flags and device permissions. | Needs verification. | Partial: related external project advertises small hardware focus; verify in PicoClaw docs. | Needs verification. | Partial: cloud deployment is documented separately from device-edge operation. | [Raspberry Pi](../hardware/raspberry-pi-setup.md), [Hardware overview](../hardware/index.md), [PicoClaw hardware list](https://github.com/sipeed/picoclaw/blob/main/docs/hardware-compatibility.md) |
| Hardware / peripheral use | Experimental: GPIO, I2C, SPI, board-specific guides, and adapter notes exist behind hardware features. | Needs verification. | Needs verification: external project. | Needs verification. | Not planned as a managed-cloud capability; hardware access belongs to local/edge hosts. | [Hardware overview](../hardware/index.md), [STM32 Nucleo](../hardware/nucleo-setup.md), [Arduino Uno Q](../hardware/arduino-uno-q-setup.md), [Aardvark](../hardware/aardvark.md), [Android](../hardware/android-setup.md) |
| Multi-instance / horizontal scaling | Not planned: one ZeroClaw instance owns one workspace; run separate instances for separate agents/workspaces. | Needs verification. | Needs verification. | Needs verification. | Partial: cloud operators can run separate instances, but not multiple writers against one workspace. | [Docker & containers](../setup/container.md), [Service & daemon](../ops/service.md) |

## Provider and model matrix

| Capability | ZeroClaw | OpenClaw | PicoClaw | NanoClaw-style lightweight runtime | Hosted/cloud shape | Evidence |
|---|---|---|---|---|---|---|
| Native provider catalog | Supported: typed provider slots and current provider families are documented in the catalog. | Needs verification. | Needs verification. | Needs verification. | Supported through hosted provider accounts configured in ZeroClaw. | [Provider catalog](../providers/catalog.md), [Providers overview](../providers/overview.md) |
| OpenAI-compatible endpoints | Supported: canonical compatible slots plus `custom` endpoints with `uri`. | Needs verification. | Needs verification. | Needs verification. | Supported when the cloud provider exposes a compatible endpoint. | [Provider catalog](../providers/catalog.md), [Custom providers](../providers/custom.md) |
| Local model providers | Supported: local and local-server provider slots are documented. | Needs verification. | Needs verification. | Needs verification. | Partial: possible on cloud hosts with local model servers, but resource requirements are operator-owned. | [Provider catalog](../providers/catalog.md), [Providers overview](../providers/overview.md) |
| Per-agent routing | Supported: agents reference providers by alias; routing is explicit rather than a single global default. | Needs verification. | Needs verification. | Needs verification. | Supported where the same config is deployed on a cloud/VPS host. | [Providers overview](../providers/overview.md), [Provider routing](../providers/routing.md) |
| Streaming text and tool calls | Supported: streaming deltas and mid-stream tool calls are runtime-supported, with provider-specific variation. | Needs verification. | Needs verification. | Needs verification. | Supported when the selected hosted provider and channel support streaming. | [Streaming](../providers/streaming.md), [Provider catalog](../providers/catalog.md) |
| Reasoning, vision, and grounded search | Partial: supported where provider/model behavior exists; coverage is not uniform across the catalog. | Needs verification. | Needs verification. | Needs verification. | Partial: depends on the hosted provider and model. | [Streaming](../providers/streaming.md), [Provider catalog](../providers/catalog.md) |

## Channel and integration matrix

| Capability | ZeroClaw | OpenClaw | PicoClaw | NanoClaw-style lightweight runtime | Hosted/cloud shape | Evidence |
|---|---|---|---|---|---|---|
| CLI channel | Supported: local stdin/stdout operation is always available. | Needs verification. | Needs verification. | Needs verification. | Not planned as a hosted-only channel. | [Channels overview](../channels/overview.md) |
| Gateway REST / WebSocket channel | Supported: gateway clients can drive the agent through HTTP/WebSocket. | Needs verification. | Needs verification. | Needs verification. | Supported with an exposed gateway and configured auth. | [Channels overview](../channels/overview.md), [Gateway HTTP API](../gateway/api.md) |
| Webhooks | Supported: webhook ingress lives under the gateway. | Needs verification. | Needs verification. | Needs verification. | Supported through reverse proxy, tunnel, or explicit public bind. | [Webhooks](../channels/webhook.md), [Network deployment](../ops/network-deployment.md) |
| Chat platforms | Partial: Matrix, Mattermost, LINE, Nextcloud Talk, Signal, WhatsApp, and other chat paths are documented, but feature depth varies by channel. | Needs verification. | Needs verification. | Needs verification. | Partial: public chat integrations usually require webhook ingress, polling, external accounts, or gateway exposure. | [Channels overview](../channels/overview.md), [Matrix](../channels/matrix.md), [Mattermost](../channels/mattermost.md), [LINE](../channels/line.md), [Nextcloud Talk](../channels/nextcloud-talk.md), [Signal](../channels/signal.md), [WhatsApp](../channels/whatsapp.md), [Other chat platforms](../channels/chat-others.md) |
| Email, social, voice, and telephony | Partial: email and social channels are documented; voice/telephony is experimental and account/deployment sensitive. | Needs verification. | Needs verification. | Needs verification. | Partial: these integrations generally require external accounts and reachable ingress. | [Email](../channels/email.md), [Social channels](../channels/social.md), [Voice & telephony](../channels/voice.md), [Network deployment](../ops/network-deployment.md) |
| ACP editor / IDE sessions | Supported: ACP works over stdio and daemon gateway WebSocket with persisted sessions and permission prompts. | Needs verification. | Needs verification. | Needs verification. | Partial: remote use depends on gateway transport and editor/client setup. | [ACP channel](../channels/acp.md) |
| Channel pairing and allow lists | Supported: pairing and authorization run before public channel events reach the agent runtime. | Needs verification. | Needs verification. | Needs verification. | Supported when public ingress is configured correctly. | [Security overview](../security/overview.md), [Channels overview](../channels/overview.md) |

## Tooling and extensibility matrix

| Capability | ZeroClaw | OpenClaw | PicoClaw | NanoClaw-style lightweight runtime | Hosted/cloud shape | Evidence |
|---|---|---|---|---|---|---|
| Built-in shell, file, HTTP, and browser tools | Supported: built-in tool families are documented, with workspace and sandbox constraints. | Needs verification. | Needs verification. | Needs verification. | Supported where host policy allows the tools. | [Tools overview](../tools/overview.md), [Browser automation](../tools/browser.md), [Security overview](../security/overview.md) |
| Memory, SOP, and cron tools | Supported: memory tools, SOP-backed tools, and cron job tools are documented. | Needs verification. | Needs verification. | Needs verification. | Supported when storage and scheduling are configured on the deployed host. | [Tools overview](../tools/overview.md), [SOP overview](../sop/index.md), [Architecture overview](../architecture/overview.md) |
| MCP tools | Supported: ZeroClaw can connect MCP servers and load their tools at startup. | Needs verification. | Needs verification. | Needs verification. | Supported when the deployed host can run or reach the MCP server. | [MCP](../tools/mcp.md), [Tools overview](../tools/overview.md) |
| Agent Skills | Supported: local `SKILL.md` / `SKILL.toml`, list/audit/install/remove/test commands, compact loading, and opt-in open-skills loading are documented. | Needs verification. | Needs verification. | Needs verification. | Supported where the deployed workspace carries the skill files. | [Skills](../tools/skills.md), [Python skills](../tools/python-skills.md) |
| Prompt-triggered skill suggestions | Partial: cached registry suggestions exist, while plugin/package discovery and composer-time suggestions remain follow-up scope. | Needs verification. | Needs verification. | Needs verification. | Partial: same runtime status as local ZeroClaw. | [Skills](../tools/skills.md) |
| WASM and skill-only plugins | Experimental: the plugin protocol documents tool plugins and skill-only bundles; several capability surfaces remain future work. | Needs verification. | Needs verification. | Needs verification. | Planned/experimental depending on host policy and plugin capability. | [Plugin protocol](../developing/plugin-protocol.md) |

## Security and policy matrix

| Capability | ZeroClaw | OpenClaw | PicoClaw | NanoClaw-style lightweight runtime | Hosted/cloud shape | Evidence |
|---|---|---|---|---|---|---|
| Autonomy levels | Supported: autonomy levels gate which tool risks can run without operator approval. | Needs verification. | Needs verification. | Needs verification. | Supported where the same runtime policy is deployed. | [Autonomy levels](../security/autonomy.md), [Security overview](../security/overview.md) |
| Workspace and path boundaries | Supported: file and shell tools are constrained by workspace and sandbox policy. | Needs verification. | Needs verification. | Needs verification. | Supported, but cloud operators must still configure host and container boundaries. | [Security overview](../security/overview.md), [Sandboxing](../security/sandboxing.md) |
| OS sandboxing | Partial: Linux, macOS, Windows, and Docker backends differ by platform and maturity. | Needs verification. | Needs verification. | Needs verification. | Partial: container/VM isolation is operator-owned in cloud deployments. | [Sandboxing](../security/sandboxing.md), [Docker & containers](../setup/container.md) |
| Tool receipts and audit trail | Supported: tool invocations produce receipt records for auditability. | Needs verification. | Needs verification. | Needs verification. | Supported when runtime storage is retained. | [Tool receipts](../security/tool-receipts.md) |
| OTP and emergency stop gates | Supported: optional gates can require one-time codes or interrupt unsafe operation. | Needs verification. | Needs verification. | Needs verification. | Supported where the deployment preserves the required operator channel. | [Security overview](../security/overview.md) |

## Deferred comparison work

This page intentionally does not claim row-by-row OpenClaw, PicoClaw, or NanoClaw-style parity. The matrix gives ZeroClaw-owned support claims and marks other runtime cells as "needs verification" unless this repository has a current source-backed basis for a narrower statement.

Future updates should tighten individual cells only when they can link to canonical evidence. If a row cannot point to docs, an issue, an RFC/FND, or source-backed reference, keep it marked "needs verification" instead of guessing.
